//! The registry, home, and session surfaces, in the same shape as everything else.
//!
//! Twenty-four of the thirty-five registry rows answer with a
//! [`SurfaceReply`] — registry packages, subscriptions, project folders,
//! session tree nodes, semantic generations, declaration diffs. Today both
//! surfaces print those as pretty-printed JSON, which is the same failure as
//! the outline: a wire value shown to a person.
//!
//! A [`ProductView`] is the small shape all of them fit: a heading, a bounded
//! list of records, and at most one note. A record is one or two lines — a
//! title, an exact operand a caller can pass back, and a few tags — which is
//! exactly the record shape the search and shelf renderings already use, so a
//! reader learns it once.

use crate::fault::Fault;
use backend_library::{
    DeclarationChange, DeclarationRecord, DiffRecord, PackageReference, ProjectRecord,
    RegistryMetadata, RegistryPackageRecord, ReleaseRecord, SemanticVersionRecord,
    SubscriptionRecord, SurfaceReply, TreeNodeRecord, TreeOpener, TreeSubject, encode_id,
};

use crate::identity::KeyTag;

/// One product record: a title, an operand to pass back, and its tags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductRecord {
    title: String,
    operand: Option<String>,
    tags: Box<[String]>,
}

impl ProductRecord {
    /// Records one row.
    #[must_use]
    pub fn new(title: impl Into<String>, operand: Option<String>, tags: Vec<String>) -> Self {
        Self {
            title: title.into(),
            operand,
            tags: tags.into_boxed_slice(),
        }
    }

    /// Returns the readable title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the exact operand a caller passes back, when there is one.
    #[must_use]
    pub fn operand(&self) -> Option<&str> {
        self.operand.as_deref()
    }

    /// Returns the tags shown after the title.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// One rendered product answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductView {
    heading: String,
    records: Box<[ProductRecord]>,
    note: Option<String>,
    fault: Option<Fault>,
}

impl ProductView {
    /// Returns the heading naming what was asked.
    #[must_use]
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// Returns the records in reply order.
    #[must_use]
    pub fn records(&self) -> &[ProductRecord] {
        &self.records
    }

    /// Returns the one-line note, when the reply carried a scalar answer.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Returns the fault explaining a fact the configured feed does not publish.
    #[must_use]
    pub const fn fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// Records one product answer a surface assembled itself.
    ///
    /// An accepted intent is not a [`SurfaceReply`], but it is the same shape
    /// to a reader, so it uses the same value rather than a parallel one.
    #[must_use]
    pub fn assembled(heading: impl Into<String>, records: Vec<ProductRecord>) -> Self {
        Self {
            heading: heading.into(),
            records: records.into_boxed_slice(),
            note: None,
            fault: None,
        }
    }

    /// Records one product answer that is a single sentence.
    #[must_use]
    pub fn stated(heading: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            records: Box::new([]),
            note: Some(note.into()),
            fault: None,
        }
    }

    fn rows(heading: &str, records: Vec<ProductRecord>) -> Self {
        Self {
            heading: heading.to_owned(),
            records: records.into_boxed_slice(),
            note: None,
            fault: None,
        }
    }

    fn scalar(heading: &str, note: impl Into<String>) -> Self {
        Self {
            heading: heading.to_owned(),
            records: Box::new([]),
            note: Some(note.into()),
            fault: None,
        }
    }

    fn refused(heading: &str, fault: Fault) -> Self {
        Self {
            heading: heading.to_owned(),
            records: Box::new([]),
            note: None,
            fault: Some(fault),
        }
    }
}

/// Lowers one durable product reply into the shared record shape.
#[must_use]
pub fn product_view(reply: &SurfaceReply) -> ProductView {
    registry_view(reply)
        .or_else(|| home_view(reply))
        .unwrap_or_else(|| session_view(reply))
}

/// Registry, library, and semantic-generation answers.
fn registry_view(reply: &SurfaceReply) -> Option<ProductView> {
    Some(match reply {
        SurfaceReply::Read(records) => {
            ProductView::rows("read", records.iter().map(declaration_row).collect())
        }
        SurfaceReply::Diff(records) => {
            ProductView::rows("diff", records.iter().map(diff_row).collect())
        }
        SurfaceReply::Explored(records) => ProductView::rows("explore", registry_rows(records)),
        SurfaceReply::Package(records) => ProductView::rows("package", registry_rows(records)),
        SurfaceReply::IndexSearch(records) => {
            ProductView::rows("index-search", registry_rows(records))
        }
        SurfaceReply::PackageVersions(records) => {
            ProductView::rows("package-versions", registry_rows(records))
        }
        SurfaceReply::Dependents(metadata) => metadata_view("dependents", metadata),
        SurfaceReply::Owner(metadata) => metadata_view("owner", metadata),
        SurfaceReply::SemanticVersions(records) => ProductView::rows(
            "semantic-versions",
            records.iter().map(semantic_row).collect(),
        ),
        SurfaceReply::SemanticVersionSelected(record) => ProductView::rows(
            "select-semantic-version",
            vec![semantic_row(record)],
        ),
        _ => return None,
    })
}

/// Subscription and project-folder answers.
fn home_view(reply: &SurfaceReply) -> Option<ProductView> {
    Some(match reply {
        SurfaceReply::PackageProfile { latest, versions } => ProductView::rows(
            "package-profile",
            latest.as_ref().map_or_else(
                || vec![ProductRecord::new("no version recorded", None, Vec::new())],
                |record| {
                    let mut row = registry_row(record);
                    row.tags = append(&row.tags, format!("{versions} version(s)"));
                    vec![row]
                },
            ),
        ),
        SurfaceReply::Subscribed(record) => {
            ProductView::rows("subscribe", vec![subscription_row(record)])
        }
        SurfaceReply::Unsubscribed(removed) => ProductView::scalar(
            "unsubscribe",
            if *removed {
                "the subscription was removed"
            } else {
                "no subscription existed for that package"
            },
        ),
        SurfaceReply::Subscriptions(records) => ProductView::rows(
            "subscriptions",
            records.iter().map(subscription_row).collect(),
        ),
        SurfaceReply::Releases(records) => {
            ProductView::rows("releases", records.iter().map(release_row).collect())
        }
        SurfaceReply::Projects(records) => {
            ProductView::rows("projects", records.iter().map(project_row).collect())
        }
        SurfaceReply::ProjectCreated(record) => {
            ProductView::rows("project-create", vec![project_row(record)])
        }
        SurfaceReply::ProjectAdded(record) => {
            ProductView::rows("project-add", vec![project_row(record)])
        }
        SurfaceReply::ProjectRemoved(record) => {
            ProductView::rows("project-remove", vec![project_row(record)])
        }
        SurfaceReply::ProjectSynced(record) => {
            ProductView::rows("project-sync", vec![project_row(record)])
        }
        SurfaceReply::ProjectDeleted(id) => {
            ProductView::scalar("project-delete", format!("project {} was deleted", id.get()))
        }
        _ => return None,
    })
}

/// Session-tree answers.
fn session_view(reply: &SurfaceReply) -> ProductView {
    match reply {
        SurfaceReply::Tree(records) => {
            ProductView::rows("tree", records.iter().map(tree_row).collect())
        }
        SurfaceReply::TreeOpened(record) => {
            ProductView::rows("tree-open", vec![tree_row(record)])
        }
        SurfaceReply::TreeClosed(count) => {
            ProductView::scalar("tree-close", format!("{count} node(s) closed"))
        }
        other => ProductView::scalar("surface", format!("{:?}", other.id())),
    }
}

fn append(tags: &[String], extra: String) -> Box<[String]> {
    let mut all = tags.to_vec();
    all.push(extra);
    all.into_boxed_slice()
}

fn metadata_view(
    heading: &str,
    metadata: &RegistryMetadata<Box<[RegistryPackageRecord]>>,
) -> ProductView {
    match metadata {
        RegistryMetadata::Recorded(records) => ProductView::rows(heading, registry_rows(records)),
        RegistryMetadata::NotRecorded(reason) => ProductView::refused(
            heading,
            Fault::new(
                crate::fault::FaultSlug::LaneUnavailable,
                crate::fault::Operand::Text(heading.to_owned()),
                crate::fault::Cause::new(
                    crate::fault::CauseSlug::Unconfigured,
                    format!("the configured feed does not publish this fact: {}", reason.as_str()),
                ),
                crate::fault::Affordance::None,
            ),
        ),
    }
}

fn registry_rows(records: &[RegistryPackageRecord]) -> Vec<ProductRecord> {
    records.iter().map(registry_row).collect()
}

fn registry_row(record: &RegistryPackageRecord) -> ProductRecord {
    ProductRecord::new(
        format!("{} {}", record.name.as_str(), record.version.as_str()),
        Some(record.coordinate.as_str().to_owned()),
        vec![
            format!("{:?}", record.ecosystem).to_lowercase(),
            format!("{} byte(s)", record.bytes),
        ],
    )
}

fn declaration_row(record: &DeclarationRecord) -> ProductRecord {
    let tags = record.stable_id.map_or_else(
        || vec!["not found".to_owned()],
        |id| vec![format!("key {}", KeyTag::from_key(&id))],
    );
    ProductRecord::new(
        record
            .signature
            .as_ref()
            .map_or_else(|| record.label.as_str().to_owned(), |signature| {
                signature.as_str().to_owned()
            }),
        Some(record.label.as_str().to_owned()),
        tags,
    )
}

fn diff_row(record: &DiffRecord) -> ProductRecord {
    let change = match record.change {
        DeclarationChange::Added => "added",
        DeclarationChange::Removed => "removed",
        DeclarationChange::Changed => "changed",
        DeclarationChange::Indeterminate => "indeterminate",
    };
    let mut tags = vec![change.to_owned()];
    if !record.links.is_empty() {
        tags.push(format!("{} relation change(s)", record.links.len()));
    }
    ProductRecord::new(
        record.label.as_str().to_owned(),
        Some(record.label.as_str().to_owned()),
        tags,
    )
}

fn semantic_row(record: &SemanticVersionRecord) -> ProductRecord {
    let mut tags = vec![
        format!("generation {}", KeyTag::from_key(&record.generation.to_bytes())),
        format!("{} artifact(s)", record.artifacts),
        format!("{} semantic byte(s)", record.semantic_bytes),
    ];
    tags.push(if record.complete {
        "complete".to_owned()
    } else {
        "partial".to_owned()
    });
    if record.selected {
        tags.push("selected".to_owned());
    }
    ProductRecord::new(
        record.coordinate.as_str().to_owned(),
        Some(encode_id(&record.generation.to_bytes())),
        tags,
    )
}

fn subscription_row(record: &SubscriptionRecord) -> ProductRecord {
    let mut tags = Vec::with_capacity(2);
    if let Some(project) = record.project {
        tags.push(format!("project {}", project.get()));
    }
    tags.push(record.seen.as_ref().map_or_else(
        || "nothing seen yet".to_owned(),
        |seen| format!("seen {}", seen.as_str()),
    ));
    ProductRecord::new(package_title(&record.package), Some(operand(&record.package)), tags)
}

fn release_row(record: &ReleaseRecord) -> ProductRecord {
    ProductRecord::new(
        format!("{} {}", package_title(&record.package), record.version.as_str()),
        Some(operand(&record.package)),
        vec![if record.seen { "seen" } else { "new" }.to_owned()],
    )
}

fn project_row(record: &ProjectRecord) -> ProductRecord {
    let mut tags = vec![
        format!("id {}", record.id.get()),
        format!("{} member(s)", record.members.len()),
    ];
    if let Some(lockfile) = record.lockfile.as_ref() {
        tags.push(format!("lockfile {}", lockfile.as_str()));
    }
    ProductRecord::new(
        record.name.as_str().to_owned(),
        Some(record.name.as_str().to_owned()),
        tags,
    )
}

fn tree_row(record: &TreeNodeRecord) -> ProductRecord {
    let mut tags = vec![
        format!("node {}", record.id.get()),
        opener_name(&record.opener).to_owned(),
    ];
    if let Some(parent) = record.parent {
        tags.push(format!("under {}", parent.get()));
    }
    if record.active {
        tags.push("active".to_owned());
    }
    ProductRecord::new(
        format!("{} · {}", record.title.as_str(), subject_text(&record.subject)),
        Some(record.id.get().to_string()),
        tags,
    )
}

const fn opener_name(opener: &TreeOpener) -> &'static str {
    match opener {
        TreeOpener::Desktop => "desktop",
        TreeOpener::Cli => "cli",
        TreeOpener::Mcp(_) => "mcp",
    }
}

fn subject_text(subject: &TreeSubject) -> String {
    match subject {
        TreeSubject::Package(package) => format!("package {}", package.as_str()),
        TreeSubject::Declaration(text) => format!("declaration {}", text.as_str()),
        TreeSubject::Explore(query) => query.as_ref().map_or_else(
            || "explore".to_owned(),
            |query| format!("explore {}", query.as_str()),
        ),
        TreeSubject::Search(text) => format!("search {}", text.as_str()),
        TreeSubject::Owner(text) => format!("owner {}", text.as_str()),
    }
}

fn package_title(package: &PackageReference) -> String {
    package.as_str().to_owned()
}

fn operand(package: &PackageReference) -> String {
    package.as_str().to_owned()
}
