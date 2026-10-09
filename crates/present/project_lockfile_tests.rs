use super::*;
use backend_library::{
    ProductText, ProjectId, ProjectLockfileMembership, ProjectName, ProjectUnresolvedLockMember,
};

#[test]
fn large_project_display_retains_all_names_and_truthful_partial_membership() {
    let record = ProjectRecord {
        id: ProjectId::new(std::num::NonZeroU64::MIN),
        name: ProjectName::new("Large").unwrap(),
        lockfile: Some(ProductText::from_static("/project/package-lock.json")),
        members: (0..1444)
            .map(|index| PackageReference::parse(format!("pkg:npm/p{index}@1.0.0")).unwrap())
            .collect(),
        member_manifest_names: (0..1444)
            .map(|index| ProductText::new(format!("p{index}")).unwrap())
            .collect(),
        lockfile_membership: Some(ProjectLockfileMembership::Partial {
            unresolved: vec![ProjectUnresolvedLockMember {
                name: ProductText::from_static("local-root"),
                version: None,
                source: Some(ProductText::from_static("packages/root")),
                reason: ProductText::from_static(
                    "local manifest has no pinned version; add its local path explicitly",
                ),
            }]
            .into_boxed_slice(),
        }),
    };
    record.admit().unwrap();
    let reply = SurfaceReply::ProjectSynced(record);
    let view = product_view(&reply);
    assert_eq!(view.records().len(), 1);
    let row = &view.records()[0];
    assert!(row.tags().iter().any(|tag| tag == "1444 member(s)"));
    assert_eq!(
        row.tags()
            .iter()
            .filter(|tag| tag.starts_with("member "))
            .count(),
        1444
    );
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag == "partial lockfile membership: 1 unresolved source(s)")
    );
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag.contains("unresolved local-root")
                && tag.contains("add its local path explicitly"))
    );
    let bytes = serde_json::to_vec(&crate::dto::ProductDto::new(&view)).unwrap();
    assert!(bytes.len() < backend_library::MAX_REPLY_BODY);
}
