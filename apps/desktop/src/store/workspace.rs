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
use crate::presentation::fault::{self, Fault, Operand};
use crate::presentation::shelf::Shelf;
use crate::presentation::status::{self, CapabilityChip, LaneChip};
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
const HYDRATION_DELAY: Duration = Duration::from_millis(1);

/// What the last lease exchange was doing.
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
    data: PathBuf,
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

impl WorkspaceStore {
    /// Installs the first admitted root and starts the live feed.
    pub(crate) fn new(
        endpoint: Endpoint,
        project: PathBuf,
        data: PathBuf,
        mode: HostMode,
        root: ViewRoot,
        model: Model,
        transport: UnixSubscriptionTransport,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut store = Self {
            revision: short_revision(&root),
            shelf: Shelf::project(&root),
            coverage: status::lane_chips(root.coverage()),
            capabilities: Vec::new(),
            capability_totals: (0, 0, 0),
            hydrating: false,
            fault: None,
            live: None,
            endpoint,
            project,
            data,
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

    /// Returns the durable workspace directory.
    pub(crate) fn data(&self) -> &Path {
        &self.data
    }

    /// Returns the row with one stable identity.
    pub(crate) fn row(&self, id: RowId) -> Option<&Row> {
        self.root.row_ref(id)
    }

    /// Returns the children of one declaration, in producer order.
    pub(crate) fn children_of(&self, symbol: SymbolKey) -> Vec<&Row> {
        self.root
            .rows()
            .iter()
            .filter(|row| row.parent == Some(symbol))
            .collect()
    }

    /// Returns the declarations belonging to one project coordinate.
    pub(crate) fn declarations_in(&self, project: &str) -> Vec<&Row> {
        self.root
            .rows()
            .iter()
            .filter(|row| matches!(row.id, RowId::Symbol(_)))
            .filter(|row| row.label.starts_with(project))
            .collect()
    }

    /// Resolves a declaration name to a stable identity, for signature links.
    pub(crate) fn resolve_name(&self, name: &str) -> Option<SymbolKey> {
        self.root.rows().iter().find_map(|row| match row.id {
            RowId::Symbol(symbol)
                if crate::presentation::identity::Identity::parse(&row.label).name() == name =>
            {
                Some(symbol)
            }
            _ => None,
        })
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
        self.capabilities = status::capability_chips(report.capabilities());
        self.capability_totals = status::capability_totals(report.capabilities());
        if !report.coverage().is_empty() {
            self.coverage = status::lane_chips(report.coverage());
        }
    }

    fn install_root(&mut self, root: ViewRoot, cx: &mut Context<Self>) {
        let previous = self.shelf.entries().len();
        self.revision = short_revision(&root);
        self.shelf = Shelf::project(&root);
        self.coverage = status::lane_chips(root.coverage());
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
        self.settle(result, changed, hydrating, root)
    }

    fn settle(
        &mut self,
        result: Result<Phase, crate::transport::error::ClientError>,
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
        } else if changed || recovered || matches!(result, Ok(Phase::Delta | Phase::Snapshot)) {
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

/// Returns the operand a fault should blame for one project.
pub(crate) fn project_operand(name: &str, spelling: &str) -> Operand {
    Operand::Project {
        name: name.to_owned(),
        spelling: spelling.to_owned(),
    }
}
