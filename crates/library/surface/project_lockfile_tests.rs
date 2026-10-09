use super::*;

fn project() -> ProjectRecord {
    ProjectRecord {
        id: ProjectId::new(NonZeroU64::MIN),
        name: ProjectName::new("App").unwrap(),
        lockfile: None,
        lockfile_membership: None,
        members: Box::new([]),
        member_manifest_names: Box::new([]),
    }
}
fn local(text: impl Into<String>) -> PackageReference {
    PackageReference::Local(ProductText::new(text).unwrap())
}

#[test]
fn project_member_count_admits_exact_limit_and_refuses_one_more() {
    let mut record = project();
    record.members = (0..MAX_PROJECT_MEMBERS)
        .map(|index| local(format!("p{index:x}")))
        .collect();
    record
        .admit()
        .expect("exact cardinality within payload bound");
    let mut members = record.members.into_vec();
    members.push(local("extra"));
    record.members = members.into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
}

#[test]
fn project_payload_admits_exact_limit_and_refuses_one_more_byte() {
    let mut record = project();
    record.members = (0..64)
        .map(|_| local("x".repeat(MAX_PRODUCT_TEXT_BYTES)))
        .collect();
    assert_eq!(
        64 * MAX_PRODUCT_TEXT_BYTES,
        MAX_PROJECT_MEMBER_PAYLOAD_BYTES
    );
    record.admit().expect("exact aggregate payload");
    record.lockfile = Some(ProductText::from_static("/project/Cargo.lock"));
    record.lockfile_membership = Some(ProjectLockfileMembership::Partial {
        unresolved: vec![ProjectUnresolvedLockMember {
            name: ProductText::from_static("x"),
            version: None,
            source: None,
            reason: ProductText::from_static("x"),
        }]
        .into_boxed_slice(),
    });
    // Keep each field valid and make the aggregate exactly limit + 1.
    let mut members = record.members.into_vec();
    members[0] = local("x".repeat(MAX_PRODUCT_TEXT_BYTES - 1));
    record.members = members.into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::TextBound));
}

#[test]
fn project_display_names_are_empty_or_exactly_parallel_and_legacy_is_readable() {
    let mut record = project();
    record.members = vec![local("a"), local("b")].into_boxed_slice();
    record.admit().unwrap();
    record.member_manifest_names = vec![ProductText::from_static("a")].into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
    record.member_manifest_names =
        vec![ProductText::from_static("a"), ProductText::from_static("b")].into_boxed_slice();
    record.admit().unwrap();
    let mut json = serde_json::to_value(&record).unwrap();
    json.as_object_mut().unwrap().remove("lockfile_membership");
    assert_eq!(
        serde_json::from_value::<ProjectRecord>(json)
            .unwrap()
            .lockfile_membership,
        None
    );
}

#[test]
fn project_reply_accounts_for_names_partial_sources_and_json_escaping() {
    let mut record = project();
    record.members = (0..1444).map(|index| local(format!("p{index}"))).collect();
    record.member_manifest_names = (0..1444)
        .map(|_| ProductText::new("display\"\\name").unwrap())
        .collect();
    record.lockfile = Some(ProductText::from_static("/project/Cargo.lock"));
    record.lockfile_membership = Some(ProjectLockfileMembership::Partial {
        unresolved: vec![ProjectUnresolvedLockMember {
            name: ProductText::from_static("unavailable"),
            version: Some(ProductText::from_static("1.0.0")),
            source: Some(ProductText::new("source\"\\line").unwrap()),
            reason: ProductText::from_static("unindexed source"),
        }]
        .into_boxed_slice(),
    });
    let encoded = serde_json::to_vec(&record).unwrap();
    assert_eq!(serialized_json_size(&record), encoded.len());
    assert!(project_record_bound(&record) >= encoded.len());
    let reply = SurfaceReply::ProjectSynced(record);
    reply.admit(reply.id()).unwrap();
    assert!(reply.encoded_size_bound() >= serde_json::to_vec(&reply).unwrap().len());
}

#[test]
fn coverage_requires_a_selected_lockfile_source() {
    let mut record = project();
    record.lockfile_membership = Some(ProjectLockfileMembership::Complete);
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
    record.lockfile = Some(ProductText::from_static("/project/Cargo.lock"));
    record.admit().unwrap();
}
