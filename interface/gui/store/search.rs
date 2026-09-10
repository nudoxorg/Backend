//! Defines the omnibar and search results for `interface-gui`.
//! This module owns mode detection, lane coverage chips, and hit selection.
//! Its narrow surface guarantees zero rows never masquerade as no matches.

use interface_documents::Count;
use interface_identity::PackageCoordinate;
use interface_library::render::common::{degradation_slug, unavailability_slug};
use interface_search::{
    Coverage, Hit, Lane, LaneReport, LaneSet, QueryText, SearchRequest, SearchScope, SearchTerminal,
    Truncation,
};

/// How many rows one page of the results sheet moves by.
pub const PAGE_ROWS: usize = 8;

/// What the omnibar is currently being used for.
///
/// One field, three jobs, decided by the first character the reader types. There is no mode
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
    /// command registry, a leading `@name` followed by a space is a scope chip, and anything else
    /// is a search.
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

    /// The scope chip's package name, when one is set.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        match self {
            Self::Search { scope, .. } => scope.as_deref(),
            Self::Idle | Self::Commands { .. } => None,
        }
    }

    /// The text being searched or filtered by.
    #[must_use]
    pub fn query(&self) -> &str {
        match self {
            Self::Idle => "",
            Self::Commands { query } | Self::Search { query, .. } => query,
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
#[derive(Debug, Default)]
pub struct SearchStore {
    text: String,
    mode: ModeHolder,
    terminal: Option<SearchTerminal>,
    selected: usize,
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

    /// Replaces the field text and re-derives the mode.
    pub fn retype(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.mode = ModeHolder(OmnibarMode::detect(&self.text));
        self.selected = 0;
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
                ..SearchScope::default()
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
