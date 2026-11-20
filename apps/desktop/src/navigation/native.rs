//! What the SQL, CSS and Impact pages are about.
//!
//! A [`NativeSubject`] names one thing in one project: a SQL table, a CSS
//! class, a custom property, a component. It is an address, never proof: the
//! owner resolves the name on its own read and may say the project has no such
//! thing, or that it could not look.

use super::route::{OrbitRoute, Route};
use super::BrowseRoute;
use crate::core::LocalProjectId;
use std::sync::Arc;

/// The language family a subject belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeDomain {
    /// SQL schema objects and queries.
    Sql,
    /// Stylesheets.
    Css,
    /// Application code that uses the other two.
    Code,
}

impl NativeDomain {
    /// The word people use for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sql => "SQL",
            Self::Css => "CSS",
            Self::Code => "Code",
        }
    }

    /// The owner's lowercase spelling.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::Css => "css",
            Self::Code => "code",
        }
    }
}

/// The kinds of thing a change can start from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeKind {
    /// A SQL table.
    SqlTable,
    /// A SQL view.
    SqlView,
    /// A SQL column, named `table.column`.
    SqlColumn,
    /// A SQL index.
    SqlIndex,
    /// A CSS class, named without its dot.
    CssClass,
    /// A CSS id, named without its hash.
    CssId,
    /// A CSS custom property, named with its two dashes.
    CssCustomProperty,
    /// A CSS selector as written.
    CssSelector,
    /// A CSS at-rule, for example `media`.
    CssAtRule,
    /// A stylesheet file.
    CssStylesheet,
    /// A component in application code.
    CodeComponent,
    /// A source file in application code.
    CodeFile,
}

impl NativeKind {
    /// Every kind.
    pub const ALL: [Self; 12] = [
        Self::SqlTable,
        Self::SqlView,
        Self::SqlColumn,
        Self::SqlIndex,
        Self::CssClass,
        Self::CssId,
        Self::CssCustomProperty,
        Self::CssSelector,
        Self::CssAtRule,
        Self::CssStylesheet,
        Self::CodeComponent,
        Self::CodeFile,
    ];

    /// The family it belongs to.
    #[must_use]
    pub const fn domain(self) -> NativeDomain {
        match self {
            Self::SqlTable | Self::SqlView | Self::SqlColumn | Self::SqlIndex => NativeDomain::Sql,
            Self::CssClass
            | Self::CssId
            | Self::CssCustomProperty
            | Self::CssSelector
            | Self::CssAtRule
            | Self::CssStylesheet => NativeDomain::Css,
            Self::CodeComponent | Self::CodeFile => NativeDomain::Code,
        }
    }

    /// The owner's kind spelling inside its domain.
    #[must_use]
    pub const fn seed_kind(self) -> &'static str {
        match self {
            Self::SqlTable => "table",
            Self::SqlView => "view",
            Self::SqlColumn => "column",
            Self::SqlIndex => "index",
            Self::CssClass => "class",
            Self::CssId => "id",
            Self::CssCustomProperty => "custom-property",
            Self::CssSelector => "selector",
            Self::CssAtRule => "at-rule",
            Self::CssStylesheet | Self::CodeFile => "file",
            Self::CodeComponent => "component",
        }
    }

    /// What people call it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SqlTable => "SQL table",
            Self::SqlView => "SQL view",
            Self::SqlColumn => "SQL column",
            Self::SqlIndex => "SQL index",
            Self::CssClass => "CSS class",
            Self::CssId => "CSS id",
            Self::CssCustomProperty => "CSS custom property",
            Self::CssSelector => "CSS selector",
            Self::CssAtRule => "CSS at-rule",
            Self::CssStylesheet => "stylesheet",
            Self::CodeComponent => "component",
            Self::CodeFile => "source file",
        }
    }

    /// The stable persisted spelling.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::SqlTable => "sql-table",
            Self::SqlView => "sql-view",
            Self::SqlColumn => "sql-column",
            Self::SqlIndex => "sql-index",
            Self::CssClass => "css-class",
            Self::CssId => "css-id",
            Self::CssCustomProperty => "css-custom-property",
            Self::CssSelector => "css-selector",
            Self::CssAtRule => "css-at-rule",
            Self::CssStylesheet => "css-stylesheet",
            Self::CodeComponent => "code-component",
            Self::CodeFile => "code-file",
        }
    }

    /// Parses the persisted spelling.
    #[must_use]
    pub fn parse_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.wire() == value)
    }

    /// The kind the owner's impact row names, when a question can start there.
    #[must_use]
    pub fn from_node(domain: NativeDomain, kind: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.domain() == domain && candidate.seed_kind() == kind
                && !matches!(candidate, Self::CodeFile))
            .or_else(|| (domain == NativeDomain::Code && kind == "module").then_some(Self::CodeFile))
    }
}

/// Why a subject was not admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubjectError {
    /// There was no name.
    Empty,
    /// The name or file is longer than the owner accepts.
    TooLong,
    /// The name has a control character.
    ControlCharacter,
    /// The file is not a path inside the project.
    NotInProject,
}

impl std::fmt::Display for SubjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "the name is empty",
            Self::TooLong => "the name is too long",
            Self::ControlCharacter => "the name has a control character",
            Self::NotInProject => "the file is not inside the project",
        })
    }
}

impl std::error::Error for SubjectError {}

/// One named thing in one project.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeSubject {
    /// The project whose analysis answers.
    pub project: LocalProjectId,
    /// What kind of thing.
    pub kind: NativeKind,
    /// Its name, normalised to the owner's spelling (no leading dot or hash).
    pub name: Arc<str>,
    /// The project-relative file that narrows an ambiguous name.
    pub path: Option<Arc<str>>,
}

impl NativeSubject {
    /// Admits a subject. A leading `.` or `#` is dropped from a class or id,
    /// and a custom property keeps its two dashes.
    pub fn new(
        project: LocalProjectId,
        kind: NativeKind,
        name: &str,
        path: Option<&str>,
    ) -> Result<Self, SubjectError> {
        let name = name.trim();
        let name = match kind {
            NativeKind::CssClass => name.strip_prefix('.').unwrap_or(name),
            NativeKind::CssId => name.strip_prefix('#').unwrap_or(name),
            _ => name,
        };
        if name.is_empty() || (kind == NativeKind::CssCustomProperty && name.len() <= 2) {
            return Err(SubjectError::Empty);
        }
        if name.len() > 512 {
            return Err(SubjectError::TooLong);
        }
        if name.chars().any(char::is_control) {
            return Err(SubjectError::ControlCharacter);
        }
        let name = if kind == NativeKind::CssCustomProperty && !name.starts_with("--") {
            format!("--{name}")
        } else {
            name.to_owned()
        };
        let path = match path.map(str::trim).filter(|path| !path.is_empty()) {
            None => None,
            Some(path) => {
                if path.len() > 1024 {
                    return Err(SubjectError::TooLong);
                }
                if path.chars().any(char::is_control)
                    || path.starts_with('/')
                    || path.contains('\\')
                    || path.split('/').any(|part| matches!(part, "" | ".." | "."))
                {
                    return Err(SubjectError::NotInProject);
                }
                Some(Arc::<str>::from(path))
            }
        };
        Ok(Self { project, kind, name: Arc::from(name), path })
    }

    /// The name as it is written in source: `.card`, `#hero`, `--brand`, `users.id`.
    #[must_use]
    pub fn written(&self) -> String {
        match self.kind {
            NativeKind::CssClass => format!(".{}", self.name),
            NativeKind::CssId => format!("#{}", self.name),
            NativeKind::CssAtRule => format!("@{}", self.name.trim_start_matches('@')),
            _ => self.name.to_string(),
        }
    }

    /// The table a column belongs to, as a table subject.
    #[must_use]
    pub fn table_of_column(&self) -> Option<Self> {
        if self.kind != NativeKind::SqlColumn {
            return None;
        }
        let (table, _) = self.name.rsplit_once('.')?;
        Self::new(self.project.clone(), NativeKind::SqlTable, table, None).ok()
    }

    /// The thing a person on `route` means by "what changes if I change
    /// this": the SQL or CSS subject of a page, or a component of a local
    /// project named like the TypeScript or JavaScript declaration they are
    /// reading. The owner says
    /// whether the project has such a thing; this only asks.
    #[must_use]
    pub fn for_route(route: &Route) -> Option<Self> {
        match route {
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Sql(subject))) => Some(subject.table_of_column().unwrap_or_else(|| subject.clone())),
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Css(subject))) => Some(subject.clone()),
            Route::Symbol(symbol) => {
                let package = symbol.package.as_str();
                if !std::path::Path::new(package).is_absolute() {
                    return None;
                }
                // Components are what the owner follows into CSS and SQL; a
                // declaration in another language has no such question yet.
                let file = symbol.id.as_str().strip_prefix(package)?.strip_prefix("::")?.split("::").next()?;
                let file = file.rsplit_once(':').map_or(file, |(file, _)| file);
                let extension = std::path::Path::new(file).extension()?.to_str()?.to_ascii_lowercase();
                if !matches!(extension.as_str(), "tsx" | "jsx" | "ts" | "js" | "mjs" | "vue") {
                    return None;
                }
                let project = LocalProjectId::new(package).ok()?;
                let declaration = crate::model::pages::SymbolRef::new(symbol.id.as_str()).ok()?;
                Self::new(project, NativeKind::CodeComponent, declaration.identity().name(), None).ok()
            }
            Route::Orbit(_) | Route::Package(_) | Route::CargoSource(_) | Route::World => None,
        }
    }

    /// The project's folder name, as people say it.
    #[must_use]
    pub fn project_name(&self) -> String {
        self.project
            .path()
            .file_name()
            .map_or_else(|| self.project.display_lossy(), |name| name.to_string_lossy().into_owned())
    }

    /// Whether the kind has a page of its own (tables, views and columns share
    /// the SQL page; classes and custom properties share the CSS page).
    #[must_use]
    pub fn has_page(&self) -> bool {
        matches!(
            self.kind,
            NativeKind::SqlTable
                | NativeKind::SqlView
                | NativeKind::SqlColumn
                | NativeKind::CssClass
                | NativeKind::CssCustomProperty
        )
    }
}

/// The fewest rows a first Impact page asks for is the owner's default; each
/// further page is the same size. Five pages are the most one view holds.
pub const IMPACT_MAX_PAGES: u8 = 5;

/// The Impact page's address: its subject and how many pages of rows it holds.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ImpactRoute {
    /// What is being changed.
    pub subject: NativeSubject,
    /// How many pages of rows the view holds (1 to [`IMPACT_MAX_PAGES`]).
    pub pages: u8,
}

impl ImpactRoute {
    /// The first page of the question.
    #[must_use]
    pub fn new(subject: NativeSubject) -> Self {
        Self { subject, pages: 1 }
    }

    /// The same question with one more page of rows, while the view can hold it.
    #[must_use]
    pub fn more(&self) -> Option<Self> {
        (self.pages < IMPACT_MAX_PAGES).then(|| Self { subject: self.subject.clone(), pages: self.pages + 1 })
    }

    /// Admits a persisted page count.
    #[must_use]
    pub fn with_pages(mut self, pages: u8) -> Self {
        self.pages = pages.clamp(1, IMPACT_MAX_PAGES);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> LocalProjectId {
        LocalProjectId::new("/workspace/shop").expect("project")
    }

    #[test]
    fn names_are_normalised_to_the_owners_spelling() {
        let class = NativeSubject::new(project(), NativeKind::CssClass, " .card ", None).expect("class");
        assert_eq!(&*class.name, "card");
        assert_eq!(class.written(), ".card");
        let property = NativeSubject::new(project(), NativeKind::CssCustomProperty, "brand", None).expect("property");
        assert_eq!(&*property.name, "--brand");
        assert_eq!(property.written(), "--brand");
        let column = NativeSubject::new(project(), NativeKind::SqlColumn, "users.id", None).expect("column");
        assert_eq!(column.table_of_column().expect("table").name.as_ref(), "users");
        assert!(NativeSubject::new(project(), NativeKind::CssCustomProperty, "--", None).is_err());
    }

    #[test]
    fn a_subject_refuses_what_the_owner_would() {
        assert_eq!(NativeSubject::new(project(), NativeKind::SqlTable, "  ", None), Err(SubjectError::Empty));
        assert_eq!(
            NativeSubject::new(project(), NativeKind::SqlTable, &"x".repeat(513), None),
            Err(SubjectError::TooLong)
        );
        assert_eq!(
            NativeSubject::new(project(), NativeKind::SqlTable, "a\nb", None),
            Err(SubjectError::ControlCharacter)
        );
        for path in ["/etc/passwd", "../x.css", "a/../b.css", "a\\b.css", "a//b.css"] {
            assert_eq!(
                NativeSubject::new(project(), NativeKind::CssClass, "a", Some(path)),
                Err(SubjectError::NotInProject),
                "{path}"
            );
        }
        assert!(NativeSubject::new(project(), NativeKind::CssClass, "a", Some("src/a.css")).is_ok());
    }

    #[test]
    fn every_kind_round_trips_through_its_persisted_spelling() {
        for kind in NativeKind::ALL {
            assert_eq!(NativeKind::parse_wire(kind.wire()), Some(kind));
        }
        assert_eq!(NativeKind::parse_wire("nonsense"), None);
    }

    #[test]
    fn impact_pages_are_bounded() {
        let subject = NativeSubject::new(project(), NativeKind::SqlTable, "users", None).expect("table");
        let mut route = ImpactRoute::new(subject);
        for _ in 1..IMPACT_MAX_PAGES {
            route = route.more().expect("room for another page");
        }
        assert_eq!(route.pages, IMPACT_MAX_PAGES);
        assert!(route.more().is_none());
        assert_eq!(route.clone().with_pages(0).pages, 1);
        assert_eq!(route.with_pages(200).pages, IMPACT_MAX_PAGES);
    }

    #[test]
    fn a_row_of_the_owners_names_the_kind_a_question_can_start_from() {
        assert_eq!(NativeKind::from_node(NativeDomain::Sql, "table"), Some(NativeKind::SqlTable));
        assert_eq!(NativeKind::from_node(NativeDomain::Css, "custom-property"), Some(NativeKind::CssCustomProperty));
        assert_eq!(NativeKind::from_node(NativeDomain::Code, "component"), Some(NativeKind::CodeComponent));
        assert_eq!(NativeKind::from_node(NativeDomain::Code, "module"), Some(NativeKind::CodeFile));
        assert_eq!(NativeKind::from_node(NativeDomain::Css, "rule"), None);
        assert_eq!(NativeKind::from_node(NativeDomain::Sql, "query"), None);
    }
}
