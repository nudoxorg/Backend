//! The registry as a place to browse: explore, search, and read one package.
//! Every request is one typed state — never a boolean pair — and every reply
//! is dropped unless it answers the newest question the reader asked.
//!
//! The design decision this module embodies is that a browse surface has no
//! "loading" flag and no "error" string. A request is exactly one
//! [`Loadable`]: it has not been asked, it is on the wire, it answered, or it
//! failed with a [`backend_present::Fault`] a page can draw in full. That is
//! what lets the browse page reserve geometry for a section it has not read
//! yet instead of drawing an empty box that looks like an answer, and it is
//! what stops a slow reply for `ser` from overwriting the rows for `serde`:
//! every request carries the generation it was issued at, and an install whose
//! generation is no longer the newest is discarded rather than applied.
//!
//! Nothing here invents a fact. The ecosystem filter, the newest-first version
//! order, and the human byte sizes are projections of what the engine returned;
//! `not recorded` is carried through as itself, because a feed that does not
//! publish reverse dependencies is a different statement from a package that
//! has none.

use super::dossier::{Card, Dossier, assemble, card};
use super::events::RegistryEvent;
use super::service::{Endpoint, Outcome, Request};
use backend_library::{
    MAX_PRODUCT_ROWS, PackageReference, ProductAdmissionError, ProductText, RegistryDownloadCount,
    RegistryEcosystem, RegistryMetadata, RegistryPackageRecord, RegistryReleaseStanding,
    RegistrySecurityStanding, SurfaceCommand, SurfaceReply,
};
use backend_present::{
    Affordance, Cause, CauseSlug, Coordinate, Fault, FaultSlug, Operand, Readiness, Shelf,
};
use gpui::AppContext as _;
use gpui::{Context, Entity, EventEmitter, Subscription, Task};
use gpui_elements::editable_text::{EditableTextState, StringStorage, TextChanged};
use std::cmp::Reverse;
use std::time::Duration;

/// How long the registry field waits after a keystroke before asking.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// How many rows one browse page holds.
pub(crate) const PAGE: u16 = 24;

/// Largest row budget one browse request ever asks the engine for.
///
/// Explore and index-search take a bound and no cursor, so "more" asks for a
/// wider window rather than for an offset. The ceiling is the engine's own
/// reply bound; the assertion below fails the build if that bound moves, so
/// this window can never ask for a page the service would refuse.
const CAP: u16 = 256;

const _: () = assert!(MAX_PRODUCT_ROWS == 256, "the engine's row bound moved");

/// How many package details are cached at once.
const DETAILS: usize = 8;

/// One cached package detail: the coordinate it answers, and its state.
type Detail = (String, Loadable<Box<Package>>);

/// The state of one request this window made of the registry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Loadable<T> {
    /// Nothing has been asked for yet.
    #[default]
    Idle,
    /// A request is on the wire.
    Loading,
    /// The engine answered; the value is exactly what it said.
    Ready(T),
    /// The request failed, and the fault says how.
    Faulted(Box<Fault>),
}

impl<T> Loadable<T> {
    /// Returns the answer, when one arrived.
    pub(crate) const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Idle | Self::Loading | Self::Faulted(_) => None,
        }
    }

    /// Returns whether nothing has been asked for yet.
    pub(crate) const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Returns whether a section should reserve geometry instead of drawing.
    pub(crate) const fn is_pending(&self) -> bool {
        matches!(self, Self::Idle | Self::Loading)
    }
}

/// What the reader is asking the registry for, as one value.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Query {
    text: String,
    ecosystem: Option<RegistryEcosystem>,
    page: u16,
}

impl Query {
    /// Returns the exact text in the registry field.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the ecosystem the page is scoped to, if any.
    pub(crate) const fn ecosystem(&self) -> Option<RegistryEcosystem> {
        self.ecosystem
    }

    /// Returns the bounded row budget this page asks the engine for.
    pub(crate) const fn limit(&self) -> u16 {
        let wanted = PAGE.saturating_mul(self.page.saturating_add(1));
        if wanted > CAP { CAP } else { wanted }
    }

    /// Returns whether a further page is still within the engine's bound.
    pub(crate) const fn can_advance(&self) -> bool {
        self.limit() < CAP
    }

    /// Returns this query asking about other text, back at the first page.
    pub(crate) fn with_text(&self, text: &str) -> Self {
        Self {
            text: text.to_owned(),
            ecosystem: self.ecosystem,
            page: 0,
        }
    }

    /// Returns this query scoped to one ecosystem, back at the first page.
    pub(crate) fn with_ecosystem(&self, ecosystem: Option<RegistryEcosystem>) -> Self {
        Self {
            text: self.text.clone(),
            ecosystem,
            page: 0,
        }
    }

    /// Returns this query one page further into the same answer.
    pub(crate) fn advanced(&self) -> Self {
        Self {
            text: self.text.clone(),
            ecosystem: self.ecosystem,
            page: self.page.saturating_add(1),
        }
    }

    /// Returns the surface command this query runs.
    ///
    /// An empty field browses the catalog; text searches the local index. That
    /// is the same pair of commands `backend explore` and `backend
    /// index-search` run, so the window can never show rows the CLI would not.
    ///
    /// # Errors
    /// Returns the engine's own operand rejection when the text is not
    /// admissible product text.
    pub(crate) fn command(&self) -> Result<SurfaceCommand, ProductAdmissionError> {
        let needle = self.text.trim();
        if needle.is_empty() {
            return Ok(SurfaceCommand::Explore {
                query: None,
                limit: self.limit(),
            });
        }
        ProductText::new(needle).map(|query| SurfaceCommand::IndexSearch {
            query,
            limit: self.limit(),
        })
    }
}

/// One registry row, projected into the four things a dense list draws.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageRow {
    coordinate: String,
    name: String,
    version: String,
    ecosystem: RegistryEcosystem,
    bytes: u64,
    standing: RegistryReleaseStanding,
    downloads: RegistryDownloadCount,
    security: RegistrySecurityStanding,
}

impl PackageRow {
    /// Returns the exact pinned coordinate this row opens.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the registry-native package name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the closed ecosystem this package came from.
    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    /// Returns the short human size drawn at the end of a row.
    pub(crate) fn size(&self) -> String {
        size_label(self.bytes)
    }

    /// Returns the exact verified archive size, for a dossier to project.
    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns current registry selection policy for this release.
    pub(crate) const fn release_standing(&self) -> RegistryReleaseStanding {
        self.standing
    }

    /// Returns the latest download observation without inventing zero.
    pub(crate) const fn downloads(&self) -> &RegistryDownloadCount {
        &self.downloads
    }

    /// Returns advisory evaluation at the recorded frontier.
    pub(crate) const fn security(&self) -> RegistrySecurityStanding {
        self.security
    }

    fn of(record: &RegistryPackageRecord) -> Self {
        Self {
            coordinate: record.coordinate.as_str().to_owned(),
            name: record.name.as_str().to_owned(),
            version: record.version.as_str().to_owned(),
            ecosystem: record.ecosystem,
            bytes: record.bytes,
            standing: record.standing,
            downloads: record.downloads.clone(),
            security: record.security,
        }
    }
}

/// What the configured feed said about reverse dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Dependents {
    /// The feed publishes this fact, and these packages depend on it.
    Recorded(Vec<PackageRow>),
    /// The feed does not publish this fact, and said why.
    NotRecorded(String),
}

/// The newest record for one package name, and how many exist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Profile {
    latest: Option<PackageRow>,
    versions: u64,
}

impl Profile {
    /// Returns the newest recorded release, when one exists.
    pub(crate) const fn latest(&self) -> Option<&PackageRow> {
        self.latest.as_ref()
    }

    /// Returns how many versions the local index recorded.
    pub(crate) const fn versions(&self) -> u64 {
        self.versions
    }
}

/// Everything the registry knows about one package, gathered in one pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Package {
    record: Loadable<Vec<PackageRow>>,
    versions: Loadable<Vec<PackageRow>>,
    dependents: Loadable<Dependents>,
    profile: Loadable<Profile>,
}

impl Package {
    /// Returns the exact registry records for this pinned coordinate.
    pub(crate) const fn record(&self) -> &Loadable<Vec<PackageRow>> {
        &self.record
    }

    /// Returns every recorded version, newest first.
    pub(crate) const fn versions(&self) -> &Loadable<Vec<PackageRow>> {
        &self.versions
    }

    /// Returns the reverse dependency facts, or the absence of them.
    pub(crate) const fn dependents(&self) -> &Loadable<Dependents> {
        &self.dependents
    }

    /// Returns the newest release and the recorded version count.
    pub(crate) const fn profile(&self) -> &Loadable<Profile> {
        &self.profile
    }
}

/// How the browse grid arranges the rows it has already loaded.
///
/// This is a client-side arrangement of one loaded page, never a new request:
/// the engine takes no sort operand, so asking it again for the same window
/// would return the same rows in the same order. Two of the three orders rank
/// by a sampled number, which is why [`Self::is_sampled`] exists — the control
/// says so rather than implying the registry published a ranking.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Ordering {
    /// The order the engine answered in.
    #[default]
    Relevance,
    /// Most downloaded first.
    Downloads,
    /// Most recently published first.
    Fresh,
}

impl Ordering {
    /// Every order the grid offers, in the order the controls are drawn.
    pub(crate) const ALL: [Self; 3] = [Self::Relevance, Self::Downloads, Self::Fresh];

    /// Returns the exact words the control shows.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Downloads => "downloads",
            Self::Fresh => "recently updated",
        }
    }

    /// Returns whether this order ranks by a sampled number.
    pub(crate) const fn is_sampled(self) -> bool {
        matches!(self, Self::Downloads | Self::Fresh)
    }

    /// Returns the words the note under the grid uses for this order.
    ///
    /// A note that said "by downloads" of an order the registry never
    /// published would read as a registry ranking; naming the sample in the
    /// sentence is what keeps the convenience from becoming a claim.
    pub(crate) const fn sentence(self) -> &'static str {
        match self {
            Self::Relevance => "in the order the index answered",
            Self::Downloads => "by sampled downloads",
            Self::Fresh => "by sampled release date",
        }
    }
}

/// One browse row, with the sample facts its card draws beside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Listing {
    row: PackageRow,
    card: Card,
}

impl Listing {
    /// Pairs one recorded row with the sample card drawn beside it.
    pub(crate) const fn new(row: PackageRow, card: Card) -> Self {
        Self { row, card }
    }

    /// Returns the recorded registry row.
    pub(crate) const fn row(&self) -> &PackageRow {
        &self.row
    }

    /// Returns the sample facts the card draws.
    pub(crate) const fn card(&self) -> &Card {
        &self.card
    }
}

/// Arranges one loaded page in place, leaving relevance exactly as it came.
///
/// The sort is stable, so equal keys keep the engine's own order rather than
/// being shuffled — two packages with the same download weight stay in the
/// order the reply listed them.
pub(crate) fn arrange(listings: &mut [Listing], order: Ordering) {
    match order {
        Ordering::Relevance => (),
        Ordering::Downloads => {
            listings.sort_by_key(|listing| Reverse(listing.card().weight()));
        }
        Ordering::Fresh => {
            listings.sort_by_key(|listing| Reverse(listing.card().freshness()));
        }
    }
}

/// What the add affordance on one registry row can offer right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Standing {
    /// The shelf has never heard of this coordinate.
    Absent,
    /// An index intent is in flight or accepted with no rows yet.
    Adding,
    /// The shelf holds it and it is readable.
    OnShelf,
    /// Indexing it failed; asking again is the honest offer.
    Failed,
}

impl Standing {
    /// Returns the exact words the affordance shows.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Absent => "Add",
            Self::Adding => "Adding…",
            Self::OnShelf => "On shelf ✓",
            Self::Failed => "Re-index",
        }
    }

    /// Returns whether clicking it submits an index intent.
    pub(crate) const fn is_actionable(self) -> bool {
        matches!(self, Self::Absent | Self::Failed)
    }
}

/// Returns what the merged shelf says about one registry coordinate.
///
/// The decision is the shelf's, not this store's: a row is on the shelf when
/// the shelf says so, which is why the add button on a browse row and the mark
/// on a library row can never disagree.
pub(crate) fn add_standing(shelf: &Shelf, coordinate: &str) -> Standing {
    let held = shelf
        .entries()
        .iter()
        .find(|entry| entry.identity().coordinate().as_str() == coordinate);
    let Some(entry) = held else {
        return Standing::Absent;
    };
    match entry.readiness() {
        Readiness::Ready => Standing::OnShelf,
        Readiness::Indexing { .. } | Readiness::Requested => Standing::Adding,
        Readiness::Failed { .. } => Standing::Failed,
    }
}

/// Returns the admitted package reference one registry coordinate names.
///
/// # Errors
/// Returns the fault a package page draws when the text is not a pinned
/// package URL. A local folder is deliberately refused rather than admitted as
/// a local label: no registry records a folder, and a page that pretended
/// otherwise would draw three empty sections.
pub(crate) fn package_of(coordinate: &str) -> Result<PackageReference, Fault> {
    let text = coordinate.trim();
    if !text.starts_with("pkg:") {
        return Err(malformed(
            text,
            "a registry page needs a package URL, such as pkg:cargo/serde@1.0.0",
        ));
    }
    if !text.contains('@') {
        return Err(malformed(
            text,
            "a package URL must pin a version with @, such as pkg:cargo/serde@1.0.0",
        ));
    }
    PackageReference::parse(text)
        .map_err(|error| Fault::admission(error, Operand::Coordinate(Coordinate::new(text))))
}

/// The three parts a pinned package URL spells out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Spelling {
    ecosystem: Option<RegistryEcosystem>,
    name: String,
    version: String,
}

impl Spelling {
    /// Splits one coordinate into the parts a page header draws.
    ///
    /// A scoped npm name keeps its slash, because the name is everything
    /// between the ecosystem token and the pinned version.
    pub(crate) fn of(coordinate: &str) -> Self {
        let text = coordinate.trim();
        let body = text.strip_prefix("pkg:").unwrap_or(text);
        let (token, rest) = body.split_once('/').unwrap_or(("", body));
        let (name, version) = rest.rsplit_once('@').unwrap_or((rest, ""));
        Self {
            ecosystem: RegistryEcosystem::parse_canonical(token).ok(),
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    /// Returns the ecosystem the token named, when it named a known one.
    pub(crate) const fn ecosystem(&self) -> Option<RegistryEcosystem> {
        self.ecosystem
    }

    /// Returns the registry-native name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the pinned version, empty when the coordinate pinned none.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the short label a recently-viewed chip shows.
    pub(crate) fn label(&self) -> String {
        if self.version.is_empty() {
            return self.name.clone();
        }
        format!("{}@{}", self.name, self.version)
    }
}

/// Returns the page state one reply installs, or `None` when it is stale.
///
/// This is the whole of the staleness rule, kept out of the store so it can be
/// proven without a window: a reply is applied only when the generation it was
/// issued at is still the newest one, and the ecosystem scope is applied here
/// rather than by the engine, which takes no ecosystem operand.
pub(crate) fn install_page(
    generation: u64,
    latest: u64,
    filter: Option<RegistryEcosystem>,
    reply: &SurfaceReply,
) -> Option<Loadable<Vec<PackageRow>>> {
    if generation != latest {
        return None;
    }
    Some(Loadable::Ready(rows_of(reply, filter)))
}

/// Returns a short human size for a verified archive.
pub(crate) fn size_label(count: u64) -> String {
    const UNIT: u64 = 1024;
    if count < UNIT {
        return format!("{count} B");
    }
    let kilobytes = count / UNIT;
    if kilobytes < UNIT {
        return format!("{kilobytes} KB");
    }
    format!("{} MB", kilobytes / UNIT)
}

/// Browse state over the local registry index.
pub(crate) struct RegistryStore {
    endpoint: Endpoint,
    query: Query,
    page: Loadable<Vec<PackageRow>>,
    generation: u64,
    pending: Option<Task<()>>,
    details: Vec<Detail>,
    dossiers: Vec<(String, Box<Dossier>)>,
    cards: Vec<(String, Card)>,
    order: Ordering,
    reading: Vec<Task<()>>,
    field: Option<Entity<EditableTextState>>,
    typing: Option<Subscription>,
}

impl EventEmitter<RegistryEvent> for RegistryStore {}

impl RegistryStore {
    /// Creates an idle registry view bound to one endpoint.
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            query: Query::default(),
            page: Loadable::Idle,
            generation: 0,
            pending: None,
            details: Vec::new(),
            dossiers: Vec::new(),
            cards: Vec::new(),
            order: Ordering::default(),
            reading: Vec::new(),
            field: None,
            typing: None,
        }
    }

    /// Returns the question the visible page answers.
    pub(crate) const fn query(&self) -> &Query {
        &self.query
    }

    /// Returns the state of the visible catalog page.
    pub(crate) const fn page(&self) -> &Loadable<Vec<PackageRow>> {
        &self.page
    }

    /// Returns whether a further page is worth offering.
    pub(crate) fn can_advance(&self) -> bool {
        self.query.can_advance()
            && self
                .page
                .ready()
                .is_some_and(|rows: &Vec<PackageRow>| !rows.is_empty())
    }

    /// Returns how the loaded page is arranged.
    pub(crate) const fn order(&self) -> Ordering {
        self.order
    }

    /// Arranges the loaded page differently, without asking the engine again.
    pub(crate) fn order_by(&mut self, order: Ordering, cx: &mut Context<Self>) {
        if self.order == order {
            return;
        }
        self.order = order;
        Self::announce(cx);
    }

    /// Returns the loaded page as cards, in the arrangement the reader chose.
    pub(crate) fn listing(&self) -> Vec<Listing> {
        let mut listings: Vec<Listing> = self
            .page
            .ready()
            .map(|rows: &Vec<PackageRow>| {
                rows.iter()
                    .map(|row| Listing::new(row.clone(), self.card_of(row.coordinate())))
                    .collect()
            })
            .unwrap_or_default();
        arrange(&mut listings, self.order);
        listings
    }

    /// Returns the whole dossier for one package, once it has been asked for.
    pub(crate) fn dossier_for(&self, coordinate: &str) -> Option<&Dossier> {
        self.dossiers
            .iter()
            .find(|(held, _)| held == coordinate)
            .map(|(_, dossier)| dossier.as_ref())
    }

    /// Returns the sample card for one coordinate, from the page's own stock.
    fn card_of(&self, coordinate: &str) -> Card {
        self.cards
            .iter()
            .find(|(held, _)| held == coordinate)
            .map_or_else(|| card(coordinate), |(_, held)| held.clone())
    }

    /// Builds one card per loaded row, keeping the cards a page already had.
    ///
    /// A card is a projection of a sample dossier, so it is built once per
    /// reply rather than once per frame: a grid that re-derived twenty-four
    /// download series every frame would spend a frame's budget on arithmetic
    /// whose answer never changes.
    fn restock(&mut self) {
        let coordinates: Vec<String> = self
            .page
            .ready()
            .map(|rows: &Vec<PackageRow>| {
                rows.iter().map(|row| row.coordinate().to_owned()).collect()
            })
            .unwrap_or_default();
        let mut stocked: Vec<(String, Card)> = Vec::with_capacity(coordinates.len());
        for coordinate in coordinates {
            let held = self.card_of(&coordinate);
            stocked.push((coordinate, held));
        }
        self.cards = stocked;
    }

    /// Returns the field behind the registry search box, creating it once.
    ///
    /// The field is created here rather than in [`Self::new`] so that the
    /// window can build this store without a context; the first frame that
    /// draws the browse page is what brings the field into existence.
    pub(crate) fn field(&mut self, cx: &mut Context<Self>) -> Entity<EditableTextState> {
        if let Some(field) = self.field.clone() {
            return field;
        }
        let field = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        self.typing = Some(cx.subscribe(&field, |this, field, _: &TextChanged, cx| {
            let text = field.read(cx).as_str().to_owned();
            this.set_text(&text, cx);
        }));
        self.field = Some(field.clone());
        field
    }

    /// Asks the catalog for a first page when nothing has been asked yet.
    pub(crate) fn ensure_page(&mut self, cx: &mut Context<Self>) {
        if self.page.is_idle() {
            self.submit(false, cx);
        }
    }

    /// Scopes the visible page to one ecosystem, or to all of them.
    pub(crate) fn scope_to(
        &mut self,
        ecosystem: Option<RegistryEcosystem>,
        cx: &mut Context<Self>,
    ) {
        if self.query.ecosystem() == ecosystem {
            return;
        }
        self.query = self.query.with_ecosystem(ecosystem);
        self.submit(false, cx);
    }

    /// Asks for one more page of the same query.
    pub(crate) fn advance(&mut self, cx: &mut Context<Self>) {
        if !self.query.can_advance() {
            return;
        }
        self.query = self.query.advanced();
        self.submit(false, cx);
    }

    /// Returns what is known about one package, once it has been asked for.
    pub(crate) fn package_for(&self, coordinate: &str) -> Option<&Loadable<Box<Package>>> {
        self.details
            .iter()
            .find(|(held, _)| held == coordinate)
            .map(|(_, state)| state)
    }

    /// Reads every registry fact about one package, once per coordinate.
    pub(crate) fn read_package(&mut self, coordinate: &str, cx: &mut Context<Self>) {
        if self.package_for(coordinate).is_some() {
            return;
        }
        let reference = match package_of(coordinate) {
            Ok(reference) => reference,
            Err(fault) => {
                self.hold(coordinate, Loadable::Faulted(Box::new(fault)));
                cx.notify();
                return;
            }
        };
        self.hold(coordinate, Loadable::Loading);
        cx.notify();
        let endpoint = self.endpoint.clone();
        let wanted = coordinate.to_owned();
        let held = wanted.clone();
        self.reading.push(cx.spawn(async move |this, cx| {
            let gathered = cx
                .background_spawn(async move { gather(&endpoint, &held, &reference) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.hold(&wanted, Loadable::Ready(Box::new(gathered)));
                cx.emit(RegistryEvent::Changed);
                cx.notify();
            });
        }));
    }

    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.query.text() == text {
            return;
        }
        self.query = self.query.with_text(text);
        self.submit(true, cx);
    }

    fn submit(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let command = match self.query.command() {
            Ok(command) => command,
            Err(error) => {
                let operand = Operand::Argument("index-search".to_owned());
                self.page = Loadable::Faulted(Box::new(Fault::admission(error, operand)));
                Self::announce(cx);
                return;
            }
        };
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let filter = self.query.ecosystem();
        let endpoint = self.endpoint.clone();
        self.page = Loadable::Loading;
        Self::announce(cx);
        self.pending = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(DEBOUNCE).await;
            }
            let request = Request::Surface {
                command: Box::new(command),
            };
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let _ = this.update(cx, |this, cx| this.install(generation, filter, outcome, cx));
        }));
    }

    fn install(
        &mut self,
        generation: u64,
        filter: Option<RegistryEcosystem>,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        let installed = match outcome {
            Ok(Outcome::Surface(reply)) => {
                install_page(generation, self.generation, filter, &reply)
            }
            Ok(_) => Some(Loadable::Faulted(Box::new(
                crate::presentation::fault::wrong_shape(
                    Operand::Argument("explore".to_owned()),
                    "a registry page",
                ),
            ))),
            Err(error) => Some(Loadable::Faulted(Box::new(Fault::from_client_error(
                &error,
                Operand::Argument("explore".to_owned()),
            )))),
        };
        let Some(state) = installed else {
            return;
        };
        if generation != self.generation {
            return;
        }
        self.page = state;
        self.restock();
        Self::announce(cx);
    }

    fn announce(cx: &mut Context<Self>) {
        cx.emit(RegistryEvent::Changed);
        cx.notify();
    }

    /// Holds one package read, and the dossier assembled from it.
    ///
    /// The dossier is assembled here rather than in the view so that a frame
    /// draws a value instead of computing one, and so that the sample sections
    /// are identical for every frame of one reply.
    fn hold(&mut self, coordinate: &str, state: Loadable<Box<Package>>) {
        let dossier = Box::new(assemble(coordinate, &state));
        self.details.retain(|(held, _)| held != coordinate);
        self.details.insert(0, (coordinate.to_owned(), state));
        self.details.truncate(DETAILS);
        self.dossiers.retain(|(held, _)| held != coordinate);
        self.dossiers.insert(0, (coordinate.to_owned(), dossier));
        self.dossiers.truncate(DETAILS);
    }
}

/// Reads all four registry sections for one package on a worker thread.
fn gather(endpoint: &Endpoint, coordinate: &str, package: &PackageReference) -> Package {
    Package {
        record: section(
            endpoint,
            coordinate,
            SurfaceCommand::Package {
                package: package.clone(),
            },
            record_of,
        ),
        versions: section(
            endpoint,
            coordinate,
            SurfaceCommand::PackageVersions {
                package: package.clone(),
            },
            versions_of,
        ),
        dependents: section(
            endpoint,
            coordinate,
            SurfaceCommand::Dependents {
                package: package.clone(),
            },
            dependents_of,
        ),
        profile: section(
            endpoint,
            coordinate,
            SurfaceCommand::PackageProfile {
                package: package.clone(),
            },
            profile_of,
        ),
    }
}

/// Builds one gathered package out of four real replies, for tests.
///
/// The admitters are the same four this module uses against the live service,
/// so a test states a reply and gets exactly the [`Package`] a window would
/// hold — there is no second, hand-built shape that could disagree with the
/// one the product uses.
#[cfg(test)]
pub(crate) fn gathered(
    record: &SurfaceReply,
    versions: &SurfaceReply,
    dependents: &SurfaceReply,
    profile: &SurfaceReply,
) -> Package {
    fn admit<T>(reply: &SurfaceReply, admit: fn(&SurfaceReply) -> Option<T>) -> Loadable<T> {
        admit(reply).map_or_else(
            || shape_fault(Operand::Argument("gathered".to_owned())),
            Loadable::Ready,
        )
    }
    Package {
        record: admit(record, record_of),
        versions: admit(versions, versions_of),
        dependents: admit(dependents, dependents_of),
        profile: admit(profile, profile_of),
    }
}

fn section<T>(
    endpoint: &Endpoint,
    coordinate: &str,
    command: SurfaceCommand,
    admit: fn(&SurfaceReply) -> Option<T>,
) -> Loadable<T> {
    let request = Request::Surface {
        command: Box::new(command),
    };
    let operand = Operand::Coordinate(Coordinate::new(coordinate));
    match super::service::run(endpoint.path(), &request) {
        Ok(Outcome::Surface(reply)) => admit(&reply).map_or_else(
            || shape_fault(operand.clone()),
            |value| Loadable::Ready(value),
        ),
        Ok(_) => shape_fault(operand),
        Err(error) => Loadable::Faulted(Box::new(Fault::from_client_error(&error, operand))),
    }
}

fn shape_fault<T>(operand: Operand) -> Loadable<T> {
    Loadable::Faulted(Box::new(crate::presentation::fault::wrong_shape(
        operand,
        "a registry section",
    )))
}

fn record_of(reply: &SurfaceReply) -> Option<Vec<PackageRow>> {
    match reply {
        SurfaceReply::Package(records) => Some(records.iter().map(PackageRow::of).collect()),
        _ => None,
    }
}

fn versions_of(reply: &SurfaceReply) -> Option<Vec<PackageRow>> {
    let SurfaceReply::PackageVersions(records) = reply else {
        return None;
    };
    let mut rows: Vec<PackageRow> = records.iter().map(PackageRow::of).collect();
    rows.sort_by_key(|row| version_rank(row.version()));
    Some(rows)
}

fn dependents_of(reply: &SurfaceReply) -> Option<Dependents> {
    match reply {
        SurfaceReply::Dependents(RegistryMetadata::Recorded(records)) => Some(
            Dependents::Recorded(records.iter().map(PackageRow::of).collect()),
        ),
        SurfaceReply::Dependents(RegistryMetadata::NotRecorded(reason)) => {
            Some(Dependents::NotRecorded(reason.as_str().to_owned()))
        }
        _ => None,
    }
}

fn profile_of(reply: &SurfaceReply) -> Option<Profile> {
    match reply {
        SurfaceReply::PackageProfile { latest, versions } => Some(Profile {
            latest: latest.as_ref().map(PackageRow::of),
            versions: *versions,
        }),
        _ => None,
    }
}

fn rows_of(reply: &SurfaceReply, filter: Option<RegistryEcosystem>) -> Vec<PackageRow> {
    let records: &[RegistryPackageRecord] = match reply {
        SurfaceReply::Explored(records)
        | SurfaceReply::IndexSearch(records)
        | SurfaceReply::Package(records)
        | SurfaceReply::PackageVersions(records) => records,
        _ => &[],
    };
    records
        .iter()
        .filter(|record| filter.is_none_or(|wanted| wanted == record.ecosystem))
        .map(PackageRow::of)
        .collect()
}

/// Returns the sort key whose *ascending* order lists versions newest first.
///
/// Replies arrive in recorded order, which is not release order. Comparing the
/// dotted numeric prefix numerically puts `2.10.0` above `2.9.0` — which a
/// string sort gets wrong. Two versions carrying the same numbers are broken
/// by their spelling, so `2.10.0` sorts above `2.10.0-rc1`: a prerelease is a
/// longer spelling of the same numbers and belongs below the release it
/// precedes. A version with no numeric prefix ranks last and keeps its exact
/// spelling rather than being reordered arbitrarily.
pub(crate) fn version_rank(version: &str) -> (Reverse<Vec<u64>>, String) {
    let numbers: Vec<u64> = version
        .split(['.', '-', '+'])
        .map_while(|part| part.parse::<u64>().ok())
        .collect();
    (Reverse(numbers), version.to_owned())
}

fn malformed(text: &str, sentence: &str) -> Fault {
    Fault::new(
        FaultSlug::Usage,
        Operand::Coordinate(Coordinate::new(text)),
        Cause::new(CauseSlug::Malformed, sentence),
        Affordance::None,
    )
}
