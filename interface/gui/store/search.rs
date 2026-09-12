//! Defines the omnibar and search results for `interface-gui`.
//! This module owns mode detection, lane coverage chips, and hit selection.
//! Its narrow surface guarantees zero rows never masquerade as no matches.

use compiler_ir_vocabulary::EntityKind;
use interface_documents::Count;
use interface_identity::PackageCoordinate;
use interface_library::render::common::{degradation_slug, unavailability_slug};
use interface_search::{
    Coverage, Hit, KindSet, Lane, LaneReport, LaneSet, QueryText, SearchRequest, SearchScope,
    SearchTerminal, Truncation,
};

/// How many rows one page of the results sheet moves by.
pub const PAGE_ROWS: usize = 8;

/// What the omnibar is currently being used for.
///
/// One field, four jobs, decided by the first character the reader types. There is no mode
/// switch to find and no second field to focus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OmnibarMode {
    /// Nothing typed.
    Idle,
    /// `>` — the command registry.
    Commands {
        /// The text after the prefix.
        query: Box<str>,
    },
    /// `#` — the local registry index.
    Index {
        /// The text after the prefix.
        query: Box<str>,
    },
    /// Ordinary search, optionally scoped by a leading `@package ` chip.
    Search {
        /// The package the search is confined to, when one was named.
        scope: Option<Box<str>>,
        /// The text being searched for.
        query: Box<str>,
    },
}

impl OmnibarMode {
    /// Reads the mode straight off the raw field text.
    ///
    /// The rules are deliberately mechanical so the reader can predict them: a leading `>` is the
    /// command registry, a leading `#` is the registry index, a leading `@name` followed by a
    /// space is a scope chip, and anything else is a search.
    #[must_use]
    pub fn detect(raw: &str) -> Self {
        let text = raw.trim_start();
        if text.is_empty() {
            return Self::Idle;
        }
        if let Some(rest) = text.strip_prefix('>') {
            return Self::Commands {
                query: rest.trim_start().into(),
            };
        }
        if let Some(rest) = text.strip_prefix('#') {
            return Self::Index {
                query: rest.trim_start().into(),
            };
        }
        if let Some(rest) = text.strip_prefix('@') {
            if let Some((scope, query)) = rest.split_once(' ') {
                if !scope.is_empty() {
                    return Self::Search {
                        scope: Some(scope.into()),
                        query: query.trim_start().into(),
                    };
                }
            }
        }
        Self::Search {
            scope: None,
            query: text.into(),
        }
    }

    /// Whether the palette registry should be showing.
    #[must_use]
    pub const fn is_commands(&self) -> bool {
        matches!(self, Self::Commands { .. })
    }

    /// Whether the registry index sheet should be showing.
    #[must_use]
    pub const fn is_index(&self) -> bool {
        matches!(self, Self::Index { .. })
    }

    /// The scope chip's package name, when one is set.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        match self {
            Self::Search { scope, .. } => scope.as_deref(),
            Self::Idle | Self::Commands { .. } | Self::Index { .. } => None,
        }
    }

    /// The text being searched or filtered by.
    #[must_use]
    pub fn query(&self) -> &str {
        match self {
            Self::Idle => "",
            Self::Commands { query } | Self::Index { query } | Self::Search { query, .. } => query,
        }
    }
}

/// One lane's coverage, as the chip row draws it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneChip {
    /// The lane's own word: `exact`, `names`, `graph`, `semantic`.
    pub label: &'static str,
    /// `✓` complete, `◐` partial or degraded, `✗` did not run.
    pub glyph: &'static str,
    /// The reason or the count, in the same words every other surface uses.
    pub note: String,
    /// Whether this lane actually ran.
    pub ran: bool,
}

impl LaneChip {
    /// Projects one lane report.
    #[must_use]
    pub fn of(report: &LaneReport) -> Self {
        let (glyph, note) = match report.coverage {
            Coverage::Complete => ("✓", count_note(report.hits)),
            Coverage::Partial { searched, total } => ("◐", format!("{}/{}", searched.0, total.0)),
            Coverage::Degraded { reason } => ("◐", degradation_slug(reason).to_owned()),
            Coverage::Unavailable { reason } => ("✗", unavailability_slug(reason).to_owned()),
        };
        Self {
            label: report.lane.label(),
            glyph,
            note,
            ran: report.coverage.ran(),
        }
    }
}

fn count_note(hits: Count) -> String {
    match hits.0 {
        0 => "none".to_owned(),
        count => count.to_string(),
    }
}

/// The omnibar's text, its mode, and whatever the last search returned.
#[derive(Debug)]
pub struct SearchStore {
    text: String,
    mode: ModeHolder,
    terminal: Option<SearchTerminal>,
    selected: usize,
    index_cursor: usize,
    kinds: KindSet,
}

impl Default for SearchStore {
    fn default() -> Self {
        Self {
            text: String::new(),
            mode: ModeHolder::default(),
            terminal: None,
            selected: 0,
            index_cursor: 0,
            kinds: KindSet::BROWSABLE,
        }
    }
}

#[derive(Debug)]
struct ModeHolder(OmnibarMode);

impl Default for ModeHolder {
    fn default() -> Self {
        Self(OmnibarMode::Idle)
    }
}

impl SearchStore {
    /// Exactly what the reader typed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What that text means.
    #[must_use]
    pub const fn mode(&self) -> &OmnibarMode {
        &self.mode.0
    }

    /// The last terminal, when a search has run.
    #[must_use]
    pub const fn terminal(&self) -> Option<&SearchTerminal> {
        self.terminal.as_ref()
    }

    /// The hits on screen.
    #[must_use]
    pub fn hits(&self) -> &[Hit] {
        self.terminal.as_ref().map_or(&[], |terminal| &terminal.hits)
    }

    /// The four lane chips, which render whether or not there are any hits.
    ///
    /// An empty result with four chips explains itself; an empty result with no chips is a lie by
    /// omission about which lanes even ran.
    #[must_use]
    pub fn lanes(&self) -> Vec<LaneChip> {
        self.terminal.as_ref().map_or_else(Vec::new, |terminal| {
            terminal.lanes.iter().map(LaneChip::of).collect()
        })
    }

    /// Whether more hits exist beyond the ones on screen.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        matches!(
            self.terminal.as_ref().map(|terminal| terminal.truncation),
            Some(Truncation::Truncated { .. })
        )
    }

    /// The kind chips' state: which declaration kinds the next request admits.
    #[must_use]
    pub const fn kinds(&self) -> KindSet {
        self.kinds
    }

    /// Flips one declaration kind's chip, so the next request admits or refuses it.
    ///
    /// Toggling does not search on its own; the shell re-runs the debounced search, exactly as
    /// if the reader had retyped, so a chip click never costs more than one engine round trip.
    pub const fn toggle_kind(&mut self, kind: EntityKind) {
        self.kinds = if self.kinds.contains(kind) {
            self.kinds.without(kind)
        } else {
            self.kinds.with(kind)
        };
    }

    /// The request for the next page of the loaded search, or `None` when it is complete.
    #[must_use]
    pub fn continuation_request(&self) -> Option<SearchRequest> {
        let terminal = self.terminal.as_ref()?;
        let Truncation::Truncated { next } = terminal.truncation else {
            return None;
        };
        let mut request = terminal.request.clone();
        request.cursor = Some(next);
        Some(request)
    }

    /// Which hit is selected.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// The selected hit, when there is one.
    #[must_use]
    pub fn selection(&self) -> Option<&Hit> {
        self.hits().get(self.selected)
    }

    /// Which index row the explore sheet has selected.
    #[must_use]
    pub const fn index_cursor(&self) -> usize {
        self.index_cursor
    }

    /// Moves the index sheet's selection by one row, never below the first.
    ///
    /// The upper bound is the loaded page's row count, which this store does not hold; the shell
    /// clamps the cursor against the rows the explore slot actually carries, the same way the
    /// palette clamps its own cursor.
    pub const fn step_index_selection(&mut self, forward: bool) {
        self.index_cursor = if forward {
            self.index_cursor.saturating_add(1)
        } else {
            self.index_cursor.saturating_sub(1)
        };
    }

    /// Selects one index row by index, as a row click does.
    pub const fn select_index(&mut self, index: usize) {
        self.index_cursor = index;
    }

    /// Replaces the field text and re-derives the mode.
    pub fn retype(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.mode = ModeHolder(OmnibarMode::detect(&self.text));
        self.selected = 0;
        self.index_cursor = 0;
    }

    /// Drops the scope chip, keeping whatever was being searched for.
    pub fn clear_scope(&mut self) {
        let query = self.mode.0.query().to_owned();
        self.retype(query);
    }

    /// Confines the search to one package, replacing any existing chip.
    pub fn scope_to(&mut self, coordinate: &PackageCoordinate) {
        let query = self.mode.0.query().to_owned();
        self.retype(format!("@{} {query}", coordinate.name.as_str()));
    }

    /// Empties the field.
    pub fn clear(&mut self) {
        self.text.clear();
        self.mode = ModeHolder(OmnibarMode::Idle);
        self.terminal = None;
        self.selected = 0;
        self.index_cursor = 0;
    }

    /// The request this field would submit, or `None` when there is nothing to search for.
    #[must_use]
    pub fn request(&self, shelf: &[PackageCoordinate]) -> Option<SearchRequest> {
        let OmnibarMode::Search { scope, query } = self.mode() else {
            return None;
        };
        let text = QueryText::new(query).ok()?;
        let packages = scope.as_ref().map(|name| {
            shelf
                .iter()
                .filter(|coordinate| coordinate.name.as_str().contains(name.as_ref()))
                .cloned()
                .collect::<Box<[PackageCoordinate]>>()
        });
        Some(SearchRequest {
            text,
            scope: SearchScope {
                packages,
                kinds: self.kinds,
            },
            lanes: LaneSet::ALL,
            limit: interface_search::ResultLimit::default(),
            cursor: None,
        })
    }

    /// Folds one terminal in, keeping the selection inside the new rows.
    pub fn apply(&mut self, terminal: SearchTerminal) {
        self.selected = 0;
        self.terminal = Some(terminal);
    }

    /// Folds one continuation terminal in, appending the rows the loaded page did not hold.
    ///
    /// Rows dedupe by exact key, the same comparison the engine's own merge uses, so a row an
    /// earlier page served is never shown twice. The selection stays where the reader left it,
    /// and the truncation line becomes the continuation's own, so the sheet never offers a page
    /// that does not exist.
    pub fn apply_continuation(&mut self, terminal: SearchTerminal) {
        let Some(loaded) = self.terminal.as_ref() else {
            self.apply(terminal);
            return;
        };
        let mut hits = loaded.hits.to_vec();
        for row in &terminal.hits {
            let key = row.symbol.key();
            if hits.iter().any(|kept| kept.symbol.key() == key) {
                continue;
            }
            hits.push(row.clone());
        }
        self.selected = self.selected.min(hits.len().saturating_sub(1));
        self.terminal = Some(SearchTerminal {
            request: terminal.request,
            hits: hits.into_boxed_slice(),
            lanes: terminal.lanes,
            truncation: terminal.truncation,
        });
    }

    /// Moves the selection by one row.
    pub fn step(&mut self, forward: bool) {
        self.move_by(if forward { 1 } else { -1 });
    }

    /// Moves the selection by one page.
    pub fn page(&mut self, forward: bool) {
        let rows = i64::try_from(PAGE_ROWS).unwrap_or(8);
        self.move_by(if forward { rows } else { -rows });
    }

    /// Selects the first row.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Selects the last row.
    pub fn select_last(&mut self) {
        self.selected = self.hits().len().saturating_sub(1);
    }

    /// Selects one row by index, when it exists.
    pub fn select(&mut self, index: usize) {
        if index < self.hits().len() {
            self.selected = index;
        }
    }

    fn move_by(&mut self, delta: i64) {
        let count = self.hits().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let last = count.saturating_sub(1);
        let current = i64::try_from(self.selected).unwrap_or(0);
        let next = current.saturating_add(delta);
        self.selected = usize::try_from(next.max(0)).unwrap_or(0).min(last);
    }
}

/// Whether one lane report should read as an explanation rather than a result.
#[must_use]
pub const fn explains_emptiness(report: &LaneReport) -> bool {
    !report.coverage.ran()
}

/// Every lane the interface knows about, so a chip row is always four wide.
pub const ALL_LANES: [Lane; 4] = Lane::ALL;

#[cfg(test)]
mod tests {
    use super::*;
    use compiler_ir_vocabulary::{DeclarationFamilyId, DeclarationIdentity, VariantFingerprint};
    use interface_documents::{Name, Symbol};
    use interface_identity::{ContentKey, ExactAddress, PathSegment, SymbolPath};

    /// One hit whose identity is fully minted, so dedup has a real key to compare.
    fn hit(row: usize) -> Option<Hit> {
        let coordinate = PackageCoordinate::parse("cargo:serde@1.0.196").ok()?;
        let segment = PathSegment::new("serde", None)?;
        let path = SymbolPath::new(vec![segment]).ok()?;
        let mut variant = [0xA1; 16];
        variant[0] ^= u8::try_from(row).ok()?;
        let key = ContentKey::new(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([0x5E; 16]),
            variant: VariantFingerprint::from_raw(variant),
        });
        let name = Name::exact(format!("serde{row}").as_bytes()).ok()?;
        Some(Hit {
            symbol: Symbol {
                address: ExactAddress::mint(coordinate, path, key),
                entity: compiler_ir::EntityId::new(1),
                name,
                kind: EntityKind::Function,
                visibility: compiler_ir::Visibility::Public,
            },
            signature: None,
            summary: None,
            lane: Lane::Exact,
            score: interface_search::Score(0),
        })
    }

    /// One complete first page of `rows` rows and a continuation page that repeats one served
    /// row, exactly as an engine page can when lanes race.
    fn pages(first: usize, second: usize) -> Option<(SearchTerminal, SearchTerminal)> {
        let request = SearchRequest {
            text: QueryText::new("serde").ok()?,
            scope: SearchScope::default(),
            lanes: LaneSet::ALL,
            limit: interface_search::ResultLimit::default(),
            cursor: None,
        };
        let report = |lane| LaneReport {
            lane,
            coverage: Coverage::Complete,
            hits: Count(1),
            elapsed: None,
        };
        let first_hits: Vec<Hit> = (0..first).filter_map(hit).collect();
        let mut next_hits: Vec<Hit> = first_hits.first().cloned().into_iter().collect();
        next_hits.extend((first..first + second).filter_map(hit));
        if first_hits.len() != first || next_hits.len() != second + 1 {
            return None;
        }
        let next = u32::try_from(first).ok()?;
        let page = |hits: Vec<Hit>, cursor: Option<interface_search::Cursor>| SearchTerminal {
            request: SearchRequest { cursor, ..request.clone() },
            hits: hits.into_boxed_slice(),
            lanes: [report(Lane::Exact), report(Lane::Lexical), report(Lane::Graph), report(Lane::Semantic)],
            truncation: Truncation::Truncated {
                next: interface_search::Cursor(next),
            },
        };
        Some((
            page(first_hits, None),
            page(next_hits, Some(interface_search::Cursor(next))),
        ))
    }

    #[test]
    fn a_continuation_appends_new_rows_and_keeps_the_selection() {
        let mut store = SearchStore::default();
        let built = pages(3, 2);
        assert!(built.is_some(), "the 3+2-row page fixture must build");
        let Some((first, next)) = built else {
            return;
        };
        store.apply(first);
        store.select_last();
        assert_eq!(store.selected(), 2);

        store.apply_continuation(next);
        assert_eq!(store.hits().len(), 5, "the continuation appends its rows");
        assert_eq!(store.selected(), 2, "the selection stays where the reader left it");
    }

    #[test]
    fn a_continuation_dedupes_by_exact_key_and_replaces_the_truncation() {
        let mut store = SearchStore::default();
        let built = pages(3, 2);
        assert!(built.is_some(), "the 3+2-row page fixture must build");
        let Some((first, next)) = built else {
            return;
        };
        store.apply(first);
        let repeated = next.hits.first().cloned();
        assert!(repeated.is_some(), "the continuation fixture must carry a row");
        let Some(row) = repeated else {
            return;
        };
        store.apply_continuation(next);
        let keyed = store.hits().iter().filter(|kept| kept.symbol.key() == row.symbol.key()).count();
        assert_eq!(keyed, 1, "a served key must never appear twice");
        assert!(store.is_truncated(), "the truncation line is the continuation's own");
    }

    #[test]
    fn a_complete_terminal_offers_no_continuation_request() {
        let mut store = SearchStore::default();
        assert!(
            store.continuation_request().is_none(),
            "nothing loaded means nothing to continue"
        );
        let built = pages(3, 2);
        assert!(built.is_some(), "the 3+2-row page fixture must build");
        let Some((first, _next)) = built else {
            return;
        };
        store.apply(first);
        let request = store.continuation_request();
        assert!(request.is_some(), "a truncated page continues");
        let Some(request) = request else {
            return;
        };
        assert_eq!(request.cursor, Some(interface_search::Cursor(3)));
    }

    #[test]
    fn a_toggled_kind_is_admitted_or_refused_by_the_next_request() {
        let mut store = SearchStore::default();
        let browsable = store.kinds();
        assert!(!browsable.contains(EntityKind::Field), "fields start excluded");
        store.retype("serde");
        let shelf: [PackageCoordinate; 0] = [];
        let request = store.request(&shelf);
        assert!(request.is_some(), "a non-empty field admits its text");
        let Some(request) = request else {
            return;
        };
        assert_eq!(request.scope.kinds, browsable);

        store.toggle_kind(EntityKind::Field);
        assert!(store.kinds().contains(EntityKind::Field));
        let reopened = store.request(&shelf);
        assert!(reopened.is_some(), "the field still admits its text");
        let Some(reopened) = reopened else {
            return;
        };
        assert!(reopened.scope.kinds.contains(EntityKind::Field));

        store.toggle_kind(EntityKind::Field);
        assert!(!store.kinds().contains(EntityKind::Field), "toggling twice restores");
    }
}
