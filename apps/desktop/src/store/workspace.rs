//! The shelf, the live feed, and the engine's honest health.
//! One immutable view root at a time, advanced only by admitted events.
//! Every projection here is computed on arrival, never during a render.
//!
//! This store owns the lease loop. It backs off when nothing is happening —
//! doubling to two seconds — and drops back to eighty milliseconds the moment
//! the revision moves, which is what lets a project indexed from the CLI
//! appear in this window without the reader touching anything. It notifies the
//! window only when something actually changed, so an idle window paints once
//! and then stops.

use super::events::WorkspaceEvent;
use super::service::Endpoint;
use crate::host::lease::HostMode;
use crate::presentation::chips::{self, CapabilityChip, LaneChip};
use crate::presentation::fault;
use backend_present::{Coordinate, Fault, KeyTag, Operand, Shelf};
use crate::reducer::model::Model;
use crate::transport::unix::UnixSubscriptionTransport;
use backend_library::{HealthReport, Row, RowId, SymbolKey, ViewRoot, encode_id};
use backend_replication::LocalSubscriptionResponse;
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a subscription lease is requested for.
const LEASE_MS: u64 = 30_000;

/// Poll delay immediately after a change.
const EAGER_DELAY: Duration = Duration::from_millis(80);

/// Longest quiet-period poll delay.
const IDLE_DELAY: Duration = Duration::from_secs(2);

/// Poll delay while a bounded snapshot is being hydrated.
///
/// Hydration is the one phase where latency is worth spending on: the reader
/// is looking at reserved geometry until it finishes. Eight milliseconds is
/// faster than a frame and still leaves the core to the service doing the
/// work, where a one-millisecond loop spent more time asking than the service
/// spent answering.
const HYDRATION_DELAY: Duration = Duration::from_millis(8);

/// What the last lease exchange was doing.
///
/// The phase is kept for the fault it names, not for pacing. A batch that
/// carries no events is still a batch, so treating "the service answered
/// `Batch`" as "something changed" pinned this loop at its eager delay for the
/// life of the window — twelve full view diffs a second over every published
/// row, forever, on a window nobody was touching. Pacing is decided by whether
/// the admitted root's version actually moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Current,
    Delta,
    Snapshot,
    Control,
}

/// The shelf and the live feed behind it.
pub(crate) struct WorkspaceStore {
    endpoint: Endpoint,
    project: PathBuf,
    mode: HostMode,
    root: ViewRoot,
    shelf: Shelf,
    coverage: Vec<LaneChip>,
    capabilities: Vec<CapabilityChip>,
    capability_totals: (usize, usize, usize),
    revision: String,
    hydrating: bool,
    fault: Option<Fault>,
    live: Option<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for WorkspaceStore {}

/// Everything the live feed needs to exist: where the owner is, what it owns,
/// and the one admitted root and cursor the window starts from.
///
/// Passed as one value rather than as six arguments because these six are not
/// independent — they all come from a single successful bootstrap, and a call
/// site that could supply five of them and not the sixth would be a call site
/// that could open a window onto nothing.
pub(crate) struct EngineLink {
    /// Where the local service is listening.
    pub(crate) endpoint: Endpoint,
    /// The project this window discovered.
    pub(crate) project: PathBuf,
    /// Whether this process owns the service or attached to it.
    pub(crate) mode: HostMode,
    /// The first admitted view root.
    pub(crate) root: ViewRoot,
    /// The reducer holding that root and its cursor.
    pub(crate) model: Model,
    /// The certified subscription transport.
    pub(crate) transport: UnixSubscriptionTransport,
}

impl WorkspaceStore {
    /// Installs the first admitted root and starts the live feed.
    pub(crate) fn new(link: EngineLink, cx: &mut Context<Self>) -> Self {
        let EngineLink {
            endpoint,
            project,
            mode,
            root,
            model,
            transport,
        } = link;
        let mut store = Self {
            revision: short_revision(&root),
            shelf: backend_present::shelf_from_root(&root, revision_tag(&root)),
            coverage: chips::lane_chips(root.coverage()),
            capabilities: Vec::new(),
            capability_totals: (0, 0, 0),
            hydrating: false,
            fault: None,
            live: None,
            endpoint,
            project,
            mode,
            root,
        };
        store.live = Some(store.follow(model, transport, cx));
        store.refresh_health(cx);
        store
    }

    /// Returns the current immutable view root.
    pub(crate) const fn root(&self) -> &ViewRoot {
        &self.root
    }

    /// Returns the projected shelf.
    pub(crate) const fn shelf(&self) -> &Shelf {
        &self.shelf
    }

    /// Returns the honest lane coverage chips.
    pub(crate) fn coverage(&self) -> &[LaneChip] {
        &self.coverage
    }

    /// Returns the capability chips.
    pub(crate) fn capabilities(&self) -> &[CapabilityChip] {
        &self.capabilities
    }

    /// Returns ready, probing, and unavailable capability counts.
    pub(crate) const fn capability_totals(&self) -> (usize, usize, usize) {
        self.capability_totals
    }

    /// Returns the short revision digest shown in the status bar.
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns how many rows the visible root committed.
    pub(crate) fn row_count(&self) -> u64 {
        self.root.row_count()
    }

    /// Returns whether a bounded snapshot is being hydrated.
    pub(crate) const fn hydrating(&self) -> bool {
        self.hydrating
    }

    /// Returns the live feed's current fault, when it has one.
    pub(crate) const fn fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// Returns whether this process owns the service or attached to it.
    pub(crate) const fn mode(&self) -> HostMode {
        self.mode
    }

    /// Returns the endpoint every store reaches the service through.
    pub(crate) const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the discovered project root.
    pub(crate) fn project(&self) -> &Path {
        &self.project
    }

    /// Returns the rows a page assembly needs: the declaration and its members.
    ///
    /// [`backend_present::page_from_document`] reads the declaration's own kind
    /// out of this slice and groups everything whose parent is the declaration
    /// into member groups, so both have to be present and the order is the
    /// producer's.
    pub(crate) fn page_rows(&self, symbol: SymbolKey) -> Vec<Row> {
        self.root
            .rows()
            .iter()
            .filter(|row| row.id == RowId::Symbol(symbol) || row.parent == Some(symbol))
            .cloned()
            .collect()
    }

    /// Re-reads bounded health so the capability chips stay honest.
    pub(crate) fn refresh_health(&mut self, cx: &mut Context<Self>) {
        let endpoint = self.endpoint.clone();
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_spawn(async move {
                    super::service::run(endpoint.path(), &super::service::Request::Health)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.install_health(report, cx));
        })
        .detach();
    }

    fn install_health(
        &mut self,
        report: Result<super::service::Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Ok(super::service::Outcome::Health(report)) = report else {
            return;
        };
        self.apply_health(&report);
        cx.emit(WorkspaceEvent::CapabilitiesChanged);
        cx.notify();
    }

    fn apply_health(&mut self, report: &HealthReport) {
        self.capabilities = chips::capability_chips(report.capabilities());
        self.capability_totals = chips::capability_totals(report.capabilities());
        if !report.coverage().is_empty() {
            self.coverage = chips::lane_chips(report.coverage());
        }
    }

    fn install_root(&mut self, root: ViewRoot, cx: &mut Context<Self>) {
        let previous = self.shelf.entries().len();
        self.revision = short_revision(&root);
        self.shelf = backend_present::shelf_from_root(&root, revision_tag(&root));
        self.coverage = chips::lane_chips(root.coverage());
        self.root = root;
        if self.shelf.entries().len() == previous {
            cx.emit(WorkspaceEvent::RowsChanged);
        } else {
            cx.emit(WorkspaceEvent::ShelfChanged);
        }
        cx.notify();
    }
}

/// The live subscription loop.
impl WorkspaceStore {
    fn follow(
        &self,
        model: Model,
        transport: UnixSubscriptionTransport,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let endpoint = self.endpoint.clone();
        cx.spawn(async move |this, cx| {
            let mut state = Feed::new(model, transport, endpoint);
            loop {
                let step = state.exchange(cx).await;
                let delivered = this.update(cx, |store, cx| step.install(store, cx));
                if delivered.is_err() {
                    break;
                }
                cx.background_executor().timer(state.delay).await;
            }
        })
    }
}

/// One lease exchange's outcome, already projected.
struct Step {
    root: Option<ViewRoot>,
    hydrating: bool,
    fault: Option<Fault>,
    recovered: bool,
}

impl Step {
    fn install(self, store: &mut WorkspaceStore, cx: &mut Context<WorkspaceStore>) {
        let was_hydrating = store.hydrating;
        store.hydrating = self.hydrating;
        if let Some(root) = self.root {
            store.fault = None;
            store.install_root(root, cx);
        }
        if let Some(fault) = self.fault {
            let changed = store.fault.as_ref() != Some(&fault);
            store.fault = Some(fault.clone());
            if changed {
                cx.emit(WorkspaceEvent::Faulted(Box::new(fault)));
                cx.notify();
            }
        } else if self.recovered {
            store.fault = None;
            cx.emit(WorkspaceEvent::Recovered);
            cx.notify();
        }
        if self.hydrating != was_hydrating {
            cx.emit(WorkspaceEvent::Hydrating);
            cx.notify();
        }
    }
}

/// The retained lease state, moved between the worker and the loop.
struct Feed {
    model: Option<Model>,
    transport: Option<UnixSubscriptionTransport>,
    endpoint: Endpoint,
    lease: Option<backend_replication::LocalSubscriptionId>,
    delay: Duration,
    failing: bool,
}

impl Feed {
    fn new(model: Model, transport: UnixSubscriptionTransport, endpoint: Endpoint) -> Self {
        Self {
            model: Some(model),
            transport: Some(transport),
            endpoint,
            lease: None,
            delay: EAGER_DELAY,
            failing: false,
        }
    }

    async fn exchange(&mut self, cx: &mut gpui::AsyncApp) -> Step {
        let (Some(mut model), Some(mut transport)) = (self.model.take(), self.transport.take())
        else {
            return Step {
                root: None,
                hydrating: false,
                fault: None,
                recovered: false,
            };
        };
        let before = model.root().version();
        let mut lease = self.lease;
        let (model, transport, lease, result) = cx
            .background_spawn(async move {
                let result = poll_once(&mut model, &mut transport, &mut lease);
                (model, transport, lease, result)
            })
            .await;
        let changed = before != model.root().version();
        let hydrating = model.snapshot_pending();
        let root = changed.then(|| model.root().clone());
        self.lease = lease;
        self.model = Some(model);
        self.transport = Some(transport);
        self.settle(&result, changed, hydrating, root)
    }

    fn settle(
        &mut self,
        result: &Result<Phase, crate::transport::error::ClientError>,
        changed: bool,
        hydrating: bool,
        root: Option<ViewRoot>,
    ) -> Step {
        let recovered = result.is_ok() && self.failing;
        let fault = result.as_ref().err().map(|error| {
            fault::from_subscription(error, &self.endpoint.spelling())
        });
        self.failing = fault.is_some();
        self.delay = if hydrating {
            HYDRATION_DELAY
        } else if changed || recovered {
            EAGER_DELAY
        } else {
            self.delay.saturating_mul(2).min(IDLE_DELAY)
        };
        if self.failing {
            self.reconnect();
        }
        Step {
            root,
            hydrating,
            fault,
            recovered,
        }
    }

    /// Drops the partial hydration and reopens the endpoint after a failure.
    fn reconnect(&mut self) {
        self.lease = None;
        if let Some(model) = self.model.as_mut() {
            model.abort_snapshot();
        }
        if let Ok(reconnected) = UnixSubscriptionTransport::connect(self.endpoint.path()) {
            self.transport = Some(reconnected);
        }
    }
}

fn poll_once(
    model: &mut Model,
    transport: &mut UnixSubscriptionTransport,
    lease: &mut Option<backend_replication::LocalSubscriptionId>,
) -> Result<Phase, crate::transport::error::ClientError> {
    let response = match *lease {
        Some(held) if model.snapshot_pending() => transport.snapshot_page(
            held,
            model.snapshot_next_token().unwrap_or_default().to_vec(),
            model.credit(),
        ),
        Some(held) => transport.resume_lease(held, model.cursor(), model.credit(), LEASE_MS),
        None => transport.open_lease(Some(model.cursor()), model.credit(), LEASE_MS),
    }?;
    let phase = phase_of(&response);
    let held = response.lease();
    if let Some(peer) = transport.authenticated_peer() {
        model.reduce_lease_response_with_verifier(response, peer)?;
    } else {
        let capability = model.root().capability();
        model.reduce_lease_response(response, capability)?;
    }
    *lease = Some(held);
    Ok(phase)
}

const fn phase_of(response: &LocalSubscriptionResponse) -> Phase {
    match response {
        LocalSubscriptionResponse::Opened { .. } | LocalSubscriptionResponse::Resumed { .. } => {
            Phase::Current
        }
        LocalSubscriptionResponse::Batch { .. } => Phase::Delta,
        LocalSubscriptionResponse::SnapshotPage { .. }
        | LocalSubscriptionResponse::ResetWithRoot { .. } => Phase::Snapshot,
        LocalSubscriptionResponse::Acked { .. }
        | LocalSubscriptionResponse::Renewed { .. }
        | LocalSubscriptionResponse::Cancelled { .. } => Phase::Control,
    }
}

fn short_revision(root: &ViewRoot) -> String {
    encode_id(root.version().as_bytes())
        .chars()
        .take(10)
        .collect()
}

/// Returns the operand a fault should blame for one project or declaration.
pub(crate) fn coordinate_operand(spelling: &str) -> Operand {
    Operand::Coordinate(Coordinate::new(spelling))
}

/// Returns the abbreviated revision tag the shelf is stamped with.
fn revision_tag(root: &ViewRoot) -> KeyTag {
    KeyTag::from_key(root.version().as_bytes())
}
