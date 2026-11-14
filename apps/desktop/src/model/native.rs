//! Read models of the SQL, CSS and Impact pages.
//!
//! Every sentence a page says is prepared here, on the read worker, from the
//! owner's reply: the page only draws it. A reply that could not look, looked
//! only in part, or looked at files that have since changed says so in the
//! model, next to the rows, so that no empty list can read as "nothing".

use crate::navigation::{NativeDomain, NativeKind, NativeSubject};
use std::sync::Arc;

/// A place in one of the project's files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Site {
    /// The project-relative file.
    pub path: Arc<str>,
    /// The one-based line, when the owner placed it.
    pub line: Option<u32>,
}

impl Site {
    /// `path:line`, or the path alone when no line was placed.
    #[must_use]
    pub fn label(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.to_string(),
        }
    }
}

/// How firmly the analysis stands behind a row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Strength {
    /// Every step is proven from the source.
    Exact,
    /// At least one step is a possible relation the analysis could not prove.
    Possible,
}

impl Strength {
    /// The word on screen. Never colour alone.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Exact => "Exact",
            Self::Possible => "Possible",
        }
    }
}

/// One thing a change may affect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactRowView {
    /// Position in the whole ordered answer, stable across pages.
    pub n: u32,
    /// Steps from the thing being changed.
    pub depth: u8,
    /// How firmly it is affected.
    pub strength: Strength,
    /// The language it is in.
    pub domain: NativeDomain,
    /// The owner's kind of node (`rule`, `component`, `query`, ...).
    pub kind: Arc<str>,
    /// Its name.
    pub name: Arc<str>,
    /// Where it is, when it has a place.
    pub site: Option<Site>,
    /// The component, function or rule it sits in.
    pub within: Option<Arc<str>>,
    /// The owner's name for the last relation (`css.styles-class`).
    pub relation: Arc<str>,
    /// Why, in the owner's words.
    pub why: Arc<str>,
    /// The row it was reached through (`None`: straight from the thing).
    pub via: Option<u32>,
    /// A question that can start from this row.
    pub follow: Option<NativeSubject>,
}

impl ImpactRowView {
    /// The row as a sentence for a screen reader.
    #[must_use]
    pub fn reading(&self) -> String {
        let place = self.site.as_ref().map_or(String::new(), |site| format!(", {}", site.label()));
        format!("{}: {} {}{place}", self.strength.word(), self.kind, self.name)
    }
}

/// A node named without being a row: what the question resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeView {
    /// The language it is in.
    pub domain: NativeDomain,
    /// The owner's kind.
    pub kind: Arc<str>,
    /// Its name.
    pub name: Arc<str>,
    /// Where it is.
    pub site: Option<Site>,
    /// A question that can start from this node.
    pub follow: Option<NativeSubject>,
}

/// How the owner resolved the name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeedView {
    /// The name was found and followed.
    Resolved,
    /// The name matches more than one thing; none was followed.
    Ambiguous,
    /// The analysed scope holds no such name.
    NotFound(Arc<str>),
    /// The language that owns the name could not answer.
    Unavailable(Arc<str>),
}

/// What one language contributed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainStateView {
    /// Read in full (its gaps are listed separately).
    Analyzed,
    /// Read in part.
    Partial(Vec<Arc<str>>),
    /// Not read at all.
    Unavailable(Arc<str>),
    /// No analysis for it is installed.
    NotInstalled,
}

/// One language's standing in an answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainView {
    /// The language.
    pub domain: NativeDomain,
    /// What it contributed.
    pub state: DomainStateView,
    /// How many files it read.
    pub files: u32,
}

/// One thing the analysis cannot see.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GapView {
    /// The owner's stable name for it.
    pub facet: Arc<str>,
    /// What cannot be seen.
    pub note: Arc<str>,
    /// How many sites or files it touches, when counted.
    pub count: Option<u32>,
}

impl GapView {
    /// The gap as one sentence.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self.count {
            Some(count) => format!("{} ({count}): {}", self.facet, self.note),
            None => format!("{}: {}", self.facet, self.note),
        }
    }
}

/// Whether the files the answer came from still match the index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FreshnessView {
    /// Every file checked matches.
    Current {
        /// How many were compared.
        checked: u32,
    },
    /// Some files changed or vanished since they were indexed.
    Stale {
        /// How many were compared.
        checked: u32,
        /// Which changed.
        changed: Vec<Arc<str>>,
        /// What to do about it, in the owner's words.
        action: Arc<str>,
    },
    /// It could not be checked.
    Unknown(Arc<str>),
}

/// How the list of rows ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListEnd {
    /// Every affected thing within the asked depth is listed.
    Complete,
    /// The owner has more rows, and this view can hold another page.
    More,
    /// The owner has more rows than this view holds.
    ViewFull,
    /// A computation budget stopped the owner: the list is a lower bound.
    BudgetLimited(Arc<str>),
}

/// What an answer proves, said honestly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Nothing could be searched.
    NothingSearched(Arc<str>),
    /// The name was not found in what was analysed.
    NameNotFound(Arc<str>),
    /// The name matches several things.
    Ambiguous,
    /// Rows were found.
    Affects {
        /// How many exact.
        exact: u32,
        /// How many possible.
        possible: u32,
        /// In how many files.
        files: u32,
    },
    /// No rows, and the owner proves there is nothing: complete coverage.
    ProvedNone,
    /// No rows, but that proves nothing: the reasons why.
    UnprovenNone(Vec<Arc<str>>),
}

/// The Impact page's reading of one owner answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactModel {
    /// What the question was about.
    pub subject: NativeSubject,
    /// How the name resolved.
    pub seed: SeedView,
    /// What it resolved to (or the candidates when ambiguous).
    pub seeds: Vec<NodeView>,
    /// Affected things reached by an exact path (all pages).
    pub exact: u32,
    /// Affected things reached only by possible paths (all pages).
    pub possible: u32,
    /// Distinct files among them.
    pub files: u32,
    /// Counts by node kind.
    pub by_kind: Vec<(Arc<str>, u32)>,
    /// The rows this view holds, in the owner's order.
    pub rows: Vec<ImpactRowView>,
    /// What each language contributed.
    pub domains: Vec<DomainView>,
    /// Everything the analysis cannot see.
    pub gaps: Vec<GapView>,
    /// Whether the answer still matches the files.
    pub freshness: FreshnessView,
    /// How the list ended.
    pub end: ListEnd,
    /// How many pages of rows the view holds.
    pub pages: u8,
    /// How many steps from the thing the owner followed.
    pub depth: u8,
    /// Whether the owner proves there is no impact.
    pub proves_no_impact: bool,
    /// The owner's suggested next steps.
    pub next: Vec<Arc<str>>,
}

impl ImpactModel {
    /// What the answer proves.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        match &self.seed {
            SeedView::Unavailable(why) => return Verdict::NothingSearched(why.clone()),
            SeedView::NotFound(why) => return Verdict::NameNotFound(why.clone()),
            SeedView::Ambiguous => return Verdict::Ambiguous,
            SeedView::Resolved => {}
        }
        if self.exact + self.possible > 0 {
            return Verdict::Affects { exact: self.exact, possible: self.possible, files: self.files };
        }
        if self.proves_no_impact {
            return Verdict::ProvedNone;
        }
        let mut reasons = Vec::new();
        for domain in &self.domains {
            let name = domain.domain.label();
            match &domain.state {
                DomainStateView::Analyzed => {}
                DomainStateView::Partial(why) => {
                    reasons.push(Arc::from(format!("{name} was read in part: {}", why.join(", "))));
                }
                DomainStateView::Unavailable(why) => reasons.push(Arc::from(format!("{name} could not be read: {why}"))),
                DomainStateView::NotInstalled => {
                    reasons.push(Arc::from(format!("{name} analysis is not installed in this build")));
                }
            }
        }
        for gap in &self.gaps {
            reasons.push(Arc::from(gap.sentence()));
        }
        match &self.freshness {
            FreshnessView::Current { .. } => {}
            FreshnessView::Stale { changed, .. } => reasons.push(Arc::from(format!(
                "files changed since they were indexed: {}",
                changed.join(", ")
            ))),
            FreshnessView::Unknown(why) => reasons.push(Arc::from(format!("freshness could not be checked: {why}"))),
        }
        match &self.end {
            ListEnd::BudgetLimited(budget) => reasons.push(Arc::from(format!("the search stopped at its {budget} budget"))),
            ListEnd::More | ListEnd::ViewFull | ListEnd::Complete => {}
        }
        if reasons.is_empty() {
            reasons.push(Arc::from("the owner did not certify complete coverage"));
        }
        Verdict::UnprovenNone(reasons)
    }

    /// Whether any row of the answer may be out of date.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(self.freshness, FreshnessView::Stale { .. })
    }
}

impl Verdict {
    /// The line the page leads with.
    #[must_use]
    pub fn sentence(&self, subject: &NativeSubject) -> String {
        let name = subject.written();
        match self {
            Self::NothingSearched(why) => {
                format!("Nothing could be searched for {name}: {why}. This says nothing about its impact.")
            }
            Self::NameNotFound(why) => format!(
                "No {} named {name} was found in what was analysed. {why} That is not evidence that changing it has no impact.",
                subject.kind.label()
            ),
            Self::Ambiguous => format!(
                "{name} matches more than one thing, so nothing was followed. Choose one below."
            ),
            Self::Affects { exact, possible, files } => {
                let file_word = if *files == 1 { "file" } else { "files" };
                format!("Changing {name} affects {exact} exact and {possible} possible, in {files} {file_word}.")
            }
            Self::ProvedNone => format!(
                "Nothing depends on {name}: every language was fully analysed and nothing was left unseen."
            ),
            Self::UnprovenNone(_) => format!(
                "No affected rows were found for {name}, but that does not prove it has no impact."
            ),
        }
    }
}

/// What a read of a project's files found about the answer's own sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verified {
    /// The owner re-read the source and it matches.
    Matches,
    /// The file changed after it was indexed.
    Changed,
    /// Not read (over the page's budget, or the owner could not).
    Unread,
}

/// Components, queries and code that use something: the query-time links the
/// owner draws today, behind one interface so that graph edges replace them
/// without the pages changing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsersView {
    /// What the owner could say.
    pub state: UsersState,
    /// The users, in the owner's order.
    pub rows: Vec<ImpactRowView>,
    /// How many are exact (all pages).
    pub exact: u32,
    /// How many are possible (all pages).
    pub possible: u32,
    /// Whether the list ends before every user.
    pub incomplete: bool,
    /// What the owner cannot see of the uses.
    pub gaps: Vec<GapView>,
    /// Whether they match the files.
    pub freshness: Option<FreshnessView>,
}

/// Whether the users could be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsersState {
    /// The owner answered.
    Read,
    /// The owner could not.
    Unavailable(Arc<str>),
}

impl UsersView {
    /// An answer nobody could give.
    #[must_use]
    pub fn unavailable(why: impl Into<Arc<str>>) -> Self {
        Self {
            state: UsersState::Unavailable(why.into()),
            rows: Vec::new(),
            exact: 0,
            possible: 0,
            incomplete: true,
            gaps: Vec::new(),
            freshness: None,
        }
    }
}

/// How the owner's analysis of the project stands for one language.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Coverage {
    /// Whether the analysis was read at all.
    pub analysed: bool,
    /// Whether what was read is all there is (nothing unanalysed, no gaps).
    pub complete: bool,
    /// The sentences that qualify it, said once.
    pub notes: Vec<Arc<str>>,
}

/// One schema statement that touched an object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlSite {
    /// Where it is.
    pub site: Site,
    /// What it did: `created`, `added column`, `dropped`, `renamed to x`, `altered`.
    pub action: Arc<str>,
    /// The name as written there, when the owner verified it.
    pub written: Option<Arc<str>>,
    /// Whether the object exists after the last statement in declared order.
    pub live: bool,
    /// Whether the source still matches what was indexed.
    pub verified: Verified,
}

/// One column and every statement that touched it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlColumn {
    /// Its name at the head of the declared order.
    pub name: Arc<str>,
    /// Whether it exists at the head.
    pub live: bool,
    /// The statements, in declared order.
    pub sites: Vec<SqlSite>,
}

/// One written mention of the object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlUse {
    /// Where it is.
    pub site: Site,
    /// The kind of statement it sits in.
    pub statement: Arc<str>,
    /// Whether the statement reads, writes or defines.
    pub role: Arc<str>,
    /// The column it names, when it names one.
    pub column: Option<Arc<str>>,
    /// The view, trigger or function it sits in.
    pub within: Option<Arc<str>>,
    /// Whether it is bound to something that still exists. A mention bound to
    /// a dropped object is kept, and said to be.
    pub live: bool,
    /// Whether the file is application code rather than SQL.
    pub in_code: bool,
    /// Whether the source still matches what was indexed.
    pub verified: Verified,
}

/// A statement that was not fully understood: absence near it proves nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlGapSite {
    /// Where it is.
    pub path: Arc<str>,
    /// Why it was not fully read.
    pub why: Arc<str>,
}

/// What the SQL analysis says about the object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqlObject {
    /// A table or view of this name is defined.
    Defined {
        /// Table or view.
        kind: NativeKind,
        /// Whether it exists at the head of the declared order.
        live: bool,
        /// The dialect the owner read it as.
        dialect: Arc<str>,
    },
    /// The analysis read the project and found no such object.
    NotDefined,
    /// The analysis could not be read.
    Unavailable(Arc<str>),
}

/// The SQL page's reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlModel {
    /// What was asked: a table, view or column.
    pub subject: NativeSubject,
    /// The table or view the page is about.
    pub object_name: Arc<str>,
    /// The column the person came for, when they came for one.
    pub column: Option<Arc<str>>,
    /// What the analysis says it is.
    pub object: SqlObject,
    /// The statements that define and change it, in declared order: its
    /// definition source and the migrations that touched it.
    pub definitions: Vec<SqlSite>,
    /// Its columns.
    pub columns: Vec<SqlColumn>,
    /// Where it is used.
    pub uses: Vec<SqlUse>,
    /// Whether more uses exist than listed.
    pub uses_incomplete: bool,
    /// Other things which depend on it, from the impact analysis.
    pub users: UsersView,
    /// How the analysis stands.
    pub coverage: Coverage,
    /// Statements nobody fully read.
    pub gap_sites: Vec<SqlGapSite>,
    /// Files whose source changed after indexing.
    pub changed: Vec<Arc<str>>,
}

/// One rule that mentions a class or a custom property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CssRule {
    /// Where it is.
    pub site: Site,
    /// Whether the line is the parser's approximation.
    pub approximate: bool,
    /// The selector, as the parser serialises it.
    pub selector: Arc<str>,
    /// The condition it sits under (`@media ...`), when it has one.
    pub condition: Option<Arc<str>>,
    /// The exact source spelling, when the owner re-read it.
    pub written: Option<Arc<str>>,
    /// Whether the source still matches what was indexed.
    pub verified: Verified,
}

/// A declaration of a custom property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CssDeclaration {
    /// Where it is.
    pub site: Site,
    /// Its value, as the parser serialises it.
    pub value: Option<Arc<str>>,
    /// Whether it is `!important`.
    pub important: bool,
    /// The selector of the rule it sits in.
    pub selector: Option<Arc<str>>,
    /// The exact spelling, when the owner re-read it.
    pub written: Option<Arc<str>>,
    /// Whether the source still matches what was indexed.
    pub verified: Verified,
}

/// A declaration whose value reads the custom property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CssRead {
    /// Where it is.
    pub site: Site,
    /// The property it sets.
    pub property: Arc<str>,
    /// How it reads: `var`, `env` or `attr`.
    pub how: Arc<str>,
}

/// What the CSS analysis says about the name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CssState {
    /// The analysis found it.
    Found,
    /// The analysis read the project and found nothing by this name.
    NotFound,
    /// The analysis could not be read.
    Unavailable(Arc<str>),
}

/// The CSS page's reading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CssModel {
    /// A class or a custom property.
    pub subject: NativeSubject,
    /// What the analysis says.
    pub state: CssState,
    /// The rules that mention a class.
    pub rules: Vec<CssRule>,
    /// Where a custom property is declared.
    pub declarations: Vec<CssDeclaration>,
    /// The declarations which read a custom property.
    pub reads: Vec<CssRead>,
    /// Whether more exist than listed.
    pub incomplete: bool,
    /// The components that use it.
    pub users: UsersView,
    /// How the analysis stands.
    pub coverage: Coverage,
    /// Files whose source changed after indexing.
    pub changed: Vec<Arc<str>>,
}

/// One project's SQL or CSS thing a query found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeHit {
    /// What it is, ready to open.
    pub subject: NativeSubject,
    /// Where it is defined, when the answer said.
    pub site: Option<Site>,
    /// A short qualification: `3 statements`, `a column of orders`.
    pub detail: Arc<str>,
}

/// What Find found in the projects' SQL and CSS.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeFind {
    /// Things by this name, per project.
    pub hits: Vec<NativeHit>,
    /// What could not be searched, said once each.
    pub notes: Vec<Arc<str>>,
    /// How many projects were searched.
    pub searched: usize,
    /// Whether the query was long enough to be searched at all.
    pub asked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalProjectId;

    fn subject() -> NativeSubject {
        NativeSubject::new(
            LocalProjectId::new("/workspace/shop").expect("project"),
            NativeKind::SqlTable,
            "orders",
            None,
        )
        .expect("subject")
    }

    fn model() -> ImpactModel {
        ImpactModel {
            subject: subject(),
            seed: SeedView::Resolved,
            seeds: Vec::new(),
            exact: 0,
            possible: 0,
            files: 0,
            by_kind: Vec::new(),
            rows: Vec::new(),
            domains: vec![DomainView { domain: NativeDomain::Sql, state: DomainStateView::Analyzed, files: 3 }],
            gaps: Vec::new(),
            freshness: FreshnessView::Current { checked: 3 },
            end: ListEnd::Complete,
            pages: 1,
            depth: 3,
            proves_no_impact: true,
            next: Vec::new(),
        }
    }

    #[test]
    fn nothing_is_proved_unless_the_owner_proves_it() {
        let proved = model();
        assert_eq!(proved.verdict(), Verdict::ProvedNone);
        assert!(proved.verdict().sentence(&subject()).starts_with("Nothing depends on orders"));

        let mut unproved = model();
        unproved.proves_no_impact = false;
        unproved.gaps.push(GapView { facet: "code.dynamic-class".into(), note: "class names built at run time".into(), count: Some(2) });
        unproved.domains.push(DomainView {
            domain: NativeDomain::Css,
            state: DomainStateView::Partial(vec!["unparsed files".into()]),
            files: 1,
        });
        unproved.freshness = FreshnessView::Stale { checked: 3, changed: vec!["db/001.sql".into()], action: "re-index".into() };
        unproved.end = ListEnd::BudgetLimited("scan".into());
        let Verdict::UnprovenNone(reasons) = unproved.verdict() else { panic!("not proved") };
        let all = reasons.join(" | ");
        for expected in ["CSS was read in part", "code.dynamic-class (2)", "db/001.sql", "scan budget"] {
            assert!(all.contains(expected), "{expected:?} missing from {all:?}");
        }
        assert!(unproved.verdict().sentence(&subject()).contains("does not prove"));
        assert!(unproved.is_stale());
    }

    #[test]
    fn a_name_the_owner_could_not_look_for_never_reads_as_no_impact() {
        let mut unavailable = model();
        unavailable.seed = SeedView::Unavailable("no SQL worker".into());
        let said = unavailable.verdict().sentence(&subject());
        assert!(said.contains("Nothing could be searched") && said.contains("says nothing about its impact"), "{said}");

        let mut missing = model();
        missing.seed = SeedView::NotFound("The analysed scope holds no such name.".into());
        let said = missing.verdict().sentence(&subject());
        assert!(said.contains("not evidence"), "{said}");
    }

    #[test]
    fn rows_found_are_counted_by_strength() {
        let mut found = model();
        found.exact = 2;
        found.possible = 1;
        found.files = 3;
        found.proves_no_impact = false;
        assert_eq!(found.verdict().sentence(&subject()), "Changing orders affects 2 exact and 1 possible, in 3 files.");
    }

    #[test]
    fn a_site_reads_as_path_and_line() {
        assert_eq!(Site { path: "db/001.sql".into(), line: Some(4) }.label(), "db/001.sql:4");
        assert_eq!(Site { path: "db/001.sql".into(), line: None }.label(), "db/001.sql");
    }
}
