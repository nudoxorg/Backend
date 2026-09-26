//! Lowers durable product replies into the shared record shape.

use super::{ProductRecord, ProductView};
use crate::fault::Fault;
use crate::identity::KeyTag;
use backend_library::{
    AcquisitionDecision, AdvisoryPackageDto, DeclarationChange, DeclarationRecord, DependencyFacts,
    DiffRecord, ForgeFact, ForgePackageRecord, PackageDependencyRecord, PackageReference,
    ProjectRecord, RegistryMetadata, RegistryNativeAvailability, RegistryNativeDetails,
    RegistryPackageRecord, ReleaseRecord, SemanticVersionRecord, SubscriptionRecord, SurfaceReply,
    TreeNodeRecord, TreeOpener, TreeSubject, encode_id,
};

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
        SurfaceReply::References { target, references } => ProductView::rows(
            format!("references to {}", target.as_str()).as_str(),
            references.iter().map(reference_row).collect(),
        ),
        SurfaceReply::Diff(records) => {
            ProductView::rows("diff", records.iter().map(diff_row).collect())
        }
        SurfaceReply::Explored(records) => ProductView::rows("explore", registry_rows(records)),
        SurfaceReply::Package(records) => ProductView::rows("package", registry_rows(records)),
        SurfaceReply::ForgePackageAdded(record) => {
            ProductView::rows("forge-add", vec![forge_row(record)])
        }
        SurfaceReply::ForgePackageReferenced(record) => {
            ProductView::rows("forge-reference", vec![forge_row(record)])
        }
        SurfaceReply::IndexSearch(records) => {
            ProductView::rows("index-search", registry_rows(records))
        }
        SurfaceReply::PackageVersions(records) => {
            ProductView::rows("package-versions", registry_rows(records))
        }
        SurfaceReply::Advisory(advisory) => advisory_view(advisory),
        SurfaceReply::Dependents(metadata) => metadata_view("dependents", metadata),
        SurfaceReply::Dependencies(facts) => dependency_view("dependencies", facts),
        SurfaceReply::Owner(metadata) => owner_view(metadata),
        SurfaceReply::SemanticVersions(records) => ProductView::rows(
            "semantic-versions",
            records.iter().map(semantic_row).collect(),
        ),
        SurfaceReply::SemanticVersionSelected(record) => {
            ProductView::rows("select-semantic-version", vec![semantic_row(record)])
        }
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
        SurfaceReply::ProjectDeleted(id) => ProductView::scalar(
            "project-delete",
            format!("project {} was deleted", id.get()),
        ),
        _ => return None,
    })
}

/// Session-tree answers.
fn session_view(reply: &SurfaceReply) -> ProductView {
    match reply {
        SurfaceReply::Tree(records) => {
            ProductView::rows("tree", records.iter().map(tree_row).collect())
        }
        SurfaceReply::TreeOpened(record) => ProductView::rows("tree-open", vec![tree_row(record)]),
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

fn owner_view(metadata: &RegistryMetadata<Box<[RegistryPackageRecord]>>) -> ProductView {
    match metadata {
        RegistryMetadata::Recorded(records) => {
            ProductView::rows("owner", records.iter().map(owner_row).collect())
        }
        RegistryMetadata::NotRecorded(_reason) => metadata_view("owner", metadata),
    }
}

fn owner_row(record: &RegistryPackageRecord) -> ProductRecord {
    if matches!(&record.coordinate, PackageReference::Local(_))
        && record.coordinate.as_str().contains("::")
    {
        return ProductRecord::new(
            record.name.as_str(),
            Some(record.coordinate.as_str().to_owned()),
            vec![
                format!("{:?}", record.ecosystem).to_lowercase(),
                "indexed file".to_owned(),
            ],
        );
    }
    registry_row(record)
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
                    format!(
                        "the configured feed does not publish this fact: {}",
                        reason.as_str()
                    ),
                ),
                crate::fault::Affordance::None,
            ),
        ),
    }
}

fn dependency_view(
    heading: &str,
    facts: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> ProductView {
    match facts {
        DependencyFacts::Known(records) => {
            ProductView::rows(heading, records.iter().map(dependency_row).collect())
        }
        DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
            ProductView::refused(
                heading,
                Fault::new(
                    crate::fault::FaultSlug::LaneUnavailable,
                    crate::fault::Operand::Text(heading.to_owned()),
                    crate::fault::Cause::new(
                        crate::fault::CauseSlug::Unconfigured,
                        reason.as_str().to_owned(),
                    ),
                    crate::fault::Affordance::None,
                ),
            )
        }
    }
}

fn dependency_row(record: &PackageDependencyRecord) -> ProductRecord {
    ProductRecord::new(
        format!(
            "{} {}",
            record.target.name.as_str(),
            record.target.requirement.as_str()
        ),
        record
            .target
            .resolved
            .as_ref()
            .map(|package| package.as_str().to_owned()),
        vec![
            format!("{:?}", record.scope).to_lowercase(),
            record.target.ecosystem.as_str().to_owned(),
            format!("authority: {:?}", record.evidence.authority).to_lowercase(),
        ],
    )
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
            native_metadata_tag(record),
        ],
    )
    .with_native_metadata(record.native_metadata.clone())
}

fn native_metadata_tag(record: &RegistryPackageRecord) -> String {
    match (
        &record.native_metadata.availability,
        &record.native_metadata.details,
    ) {
        (RegistryNativeAvailability::Recorded, details) => format!(
            "native: {}",
            match details {
                RegistryNativeDetails::Cargo(_) => "cargo",
                RegistryNativeDetails::Npm(_) => "npm",
                RegistryNativeDetails::Pypi(_) => "pypi",
                RegistryNativeDetails::Maven(_) => "maven",
                RegistryNativeDetails::Nuget(_) => "nuget",
                RegistryNativeDetails::Golang(_) => "go",
                RegistryNativeDetails::Cpp(_) => "conan",
                RegistryNativeDetails::Unavailable { .. } => "unavailable",
            }
        ),
        (RegistryNativeAvailability::NotRecorded(_), _) => "native: not-recorded".to_owned(),
    }
}

fn forge_row(record: &ForgePackageRecord) -> ProductRecord {
    let source = match &record.source {
        ForgeFact::Recorded(value) | ForgeFact::Unavailable(value) => value.as_str().to_owned(),
    };
    ProductRecord::new(
        format!("{} / {}", record.owner.as_str(), record.repository.as_str()),
        Some(record.coordinate.as_str().to_owned()),
        vec![
            format!("provider: {}", record.provider.as_str()),
            format!("revision: {}", record.revision.as_str()),
            format!("source: {source}"),
            format!("manifests: {}", record.manifests.len()),
        ],
    )
}

fn advisory_view(advisory: &AdvisoryPackageDto) -> ProductView {
    let decision = match &advisory.decision {
        AcquisitionDecision::Allow => "allow".to_owned(),
        AcquisitionDecision::Warn(reasons) => format!(
            "warn ({})",
            reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AcquisitionDecision::Deny(reasons) => format!(
            "deny ({})",
            reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let mut rows = vec![ProductRecord::new(
        "security decision",
        None,
        vec![
            format!("decision: {decision}"),
            format!("coverage: {:?}", advisory.coverage),
            format!("freshness: {:?}", advisory.freshness),
            format!("yanked: {}", advisory.yanked),
            format!("unlisted: {}", advisory.unlisted),
        ],
    )];
    if advisory.advisories.is_empty() {
        rows.push(ProductRecord::new(
            "no matching advisory object",
            None,
            vec!["coverage and freshness remain authoritative".to_owned()],
        ));
    } else {
        rows.extend(advisory.advisories.iter().map(|claim| {
            let sources = claim
                .source_ids
                .iter()
                .map(|source| format!("{:?}:{}", source.source, source.id))
                .collect::<Vec<_>>()
                .join(", ");
            let aliases = if claim.aliases.is_empty() {
                "none".to_owned()
            } else {
                claim.aliases.join(", ")
            };
            let affected = claim
                .affected
                .iter()
                .map(|range| format!("{}:{:?}", range.package.name, range.matcher))
                .collect::<Vec<_>>()
                .join(", ");
            let fixed = if claim.fixed_ranges.is_empty() {
                "none".to_owned()
            } else {
                claim.fixed_ranges.join(", ")
            };
            ProductRecord::new(
                claim.canonical_id.clone(),
                None,
                vec![
                    format!("sources: {sources}"),
                    format!("aliases: {aliases}"),
                    format!("severity: {:?}", claim.severity),
                    format!(
                        "categories: {}",
                        claim
                            .categories
                            .iter()
                            .map(|category| format!("{category:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!(
                        "affected: {}",
                        if affected.is_empty() {
                            "unspecified"
                        } else {
                            &affected
                        }
                    ),
                    format!("fixed: {fixed}"),
                    format!("statuses: {:?}", claim.statuses),
                ],
            )
        }));
    }
    ProductView::rows("advisory", rows)
}

fn declaration_row(record: &DeclarationRecord) -> ProductRecord {
    let tags = record.stable_id.map_or_else(
        || vec!["not found".to_owned()],
        |id| vec![format!("key {}", KeyTag::from_key(&id))],
    );
    ProductRecord::new(
        record.signature.as_ref().map_or_else(
            || record.label.as_str().to_owned(),
            |signature| signature.as_str().to_owned(),
        ),
        Some(record.label.as_str().to_owned()),
        tags,
    )
}

/// Renders one source-verified use of the queried declaration.
fn reference_row(record: &backend_library::ReferenceRecord) -> ProductRecord {
    let mut tags = vec![
        match record.relation {
            backend_library::SemanticLinkKind::Calls => "calls".to_owned(),
            backend_library::SemanticLinkKind::MethodCall => "method call".to_owned(),
            backend_library::SemanticLinkKind::TypeReference => "type reference".to_owned(),
            backend_library::SemanticLinkKind::Reads => "reads".to_owned(),
            backend_library::SemanticLinkKind::Writes => "writes".to_owned(),
            backend_library::SemanticLinkKind::Imports => "imports".to_owned(),
            backend_library::SemanticLinkKind::Implements => "implements".to_owned(),
            backend_library::SemanticLinkKind::Overrides => "overrides".to_owned(),
            backend_library::SemanticLinkKind::Reexports => "re-exports".to_owned(),
            backend_library::SemanticLinkKind::Inherits => "inherits".to_owned(),
            backend_library::SemanticLinkKind::Documents => "documents".to_owned(),
        },
        match record.evidence.confidence {
            backend_library::SemanticConfidence::Syntactic => "syntactic".to_owned(),
            backend_library::SemanticConfidence::Heuristic => "heuristic".to_owned(),
            backend_library::SemanticConfidence::Indexed => "indexed".to_owned(),
            backend_library::SemanticConfidence::Imported => "imported".to_owned(),
            backend_library::SemanticConfidence::Compiler => "compiler".to_owned(),
        },
    ];
    match record.evidence.source.as_ref() {
        Some(span) => tags.push(format!(
            "{}:{}-{}",
            span.file.as_str(),
            span.start,
            span.end
        )),
        None => tags.push("site not captured".to_owned()),
    }
    ProductRecord::new(
        record.site.as_str().to_owned(),
        Some(record.site.as_str().to_owned()),
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
        format!(
            "generation {}",
            KeyTag::from_key(&record.generation.to_bytes())
        ),
        format!("{} artifact(s)", record.artifacts),
        format!("{} semantic byte(s)", record.semantic_bytes),
    ];
    if let PackageReference::Purl(coordinate) = &record.package {
        tags.push(format!("version {}", coordinate.version()));
    }
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
    ProductRecord::new(
        package_title(&record.package),
        Some(operand(&record.package)),
        tags,
    )
}

fn release_row(record: &ReleaseRecord) -> ProductRecord {
    ProductRecord::new(
        format!(
            "{} {}",
            package_title(&record.package),
            record.version.as_str()
        ),
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
    for name in record.member_manifest_names.iter() {
        tags.push(format!("member {}", name.as_str()));
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
    if let TreeSubject::Declaration(_) = &record.subject {
        if let Some((name, path)) = record.title.as_str().split_once(" · ") {
            tags.push(format!("name {name}"));
            tags.push(format!("path {path}"));
        }
    }
    ProductRecord::new(
        format!(
            "{} · {}",
            record.title.as_str(),
            subject_text(&record.subject)
        ),
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
        TreeSubject::Declaration(text) => {
            if let Some((name, path)) = text.as_str().split_once("::").and_then(|(_, rest)| {
                rest.rsplit_once("::").map(|(path, name)| {
                    (
                        name.to_owned(),
                        path.rsplit_once(':')
                            .map_or(path, |(path_without_line, _)| path_without_line)
                            .to_owned(),
                    )
                })
            }) {
                format!("declaration {name} at {path}")
            } else {
                format!("declaration {}", text.as_str())
            }
        }
        TreeSubject::Explore(query) => query.as_ref().map_or_else(
            || "explore".to_owned(),
            |query| format!("explore {}", query.as_str()),
        ),
        TreeSubject::Search(text) => format!("search {}", text.as_str()),
        TreeSubject::Owner(text) => format!("owner {}", text.as_str()),
    }
}

fn package_title(package: &PackageReference) -> String {
    match package {
        PackageReference::Purl(url) => url.lineage_name().to_owned(),
        PackageReference::Local(label) => label.as_str().to_owned(),
    }
}

fn operand(package: &PackageReference) -> String {
    package.as_str().to_owned()
}
