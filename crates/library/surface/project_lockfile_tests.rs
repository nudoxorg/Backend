use super::*;

#[derive(Debug)]
enum ProjectLockfileTestError {
    Admission(ProductAdmissionError),
    Json(serde_json::Error),
    ExpectedObject(&'static str),
}

impl From<ProductAdmissionError> for ProjectLockfileTestError {
    fn from(error: ProductAdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl From<serde_json::Error> for ProjectLockfileTestError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl core::fmt::Display for ProjectLockfileTestError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Admission(error) => write!(formatter, "project admission failed: {error}"),
            Self::Json(error) => write!(formatter, "project JSON codec failed: {error}"),
            Self::ExpectedObject(context) => {
                write!(formatter, "expected a JSON object: {context}")
            }
        }
    }
}

impl std::error::Error for ProjectLockfileTestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Admission(_) | Self::ExpectedObject(_) => None,
        }
    }
}

fn project() -> Result<ProjectRecord, ProductAdmissionError> {
    Ok(ProjectRecord {
        id: ProjectId::new(NonZeroU64::MIN),
        name: ProjectName::new("App")?,
        lockfile: None,
        lockfile_membership: None,
        members: Box::new([]),
        member_manifest_names: Box::new([]),
    })
}
fn local(text: impl Into<String>) -> Result<PackageReference, ProductAdmissionError> {
    Ok(PackageReference::Local(ProductText::new(text)?))
}

#[test]
fn project_member_count_admits_exact_limit_and_refuses_one_more()
-> Result<(), ProjectLockfileTestError> {
    let mut record = project()?;
    record.members = (0..MAX_PROJECT_MEMBERS)
        .map(|index| local(format!("p{index:x}")))
        .collect::<Result<Box<[_]>, _>>()?;
    record.admit()?;
    let mut members = record.members.into_vec();
    members.push(local("extra")?);
    record.members = members.into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
    Ok(())
}

#[test]
fn project_payload_admits_exact_limit_and_refuses_one_more_byte()
-> Result<(), ProjectLockfileTestError> {
    let mut record = project()?;
    record.members = (0..64)
        .map(|_| local("x".repeat(MAX_PRODUCT_TEXT_BYTES)))
        .collect::<Result<Box<[_]>, _>>()?;
    assert_eq!(
        64 * MAX_PRODUCT_TEXT_BYTES,
        MAX_PROJECT_MEMBER_PAYLOAD_BYTES
    );
    record.admit()?;
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
    members[0] = local("x".repeat(MAX_PRODUCT_TEXT_BYTES - 1))?;
    record.members = members.into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::TextBound));
    Ok(())
}

#[test]
fn project_display_names_are_empty_or_exactly_parallel_and_legacy_is_readable()
-> Result<(), ProjectLockfileTestError> {
    let mut record = project()?;
    record.members = vec![local("a")?, local("b")?].into_boxed_slice();
    record.admit()?;
    record.member_manifest_names = vec![ProductText::from_static("a")].into_boxed_slice();
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
    record.member_manifest_names =
        vec![ProductText::from_static("a"), ProductText::from_static("b")].into_boxed_slice();
    record.admit()?;
    let mut json = serde_json::to_value(&record)?;
    json.as_object_mut()
        .ok_or(ProjectLockfileTestError::ExpectedObject("project JSON"))?
        .remove("lockfile_membership");
    assert_eq!(
        serde_json::from_value::<ProjectRecord>(json)?.lockfile_membership,
        None
    );
    Ok(())
}

#[test]
fn project_reply_accounts_for_names_partial_sources_and_json_escaping()
-> Result<(), ProjectLockfileTestError> {
    let mut record = project()?;
    record.members = (0..1444)
        .map(|index| local(format!("p{index}")))
        .collect::<Result<Box<[_]>, _>>()?;
    record.member_manifest_names = (0..1444)
        .map(|_| ProductText::new("display\"\\name"))
        .collect::<Result<Box<[_]>, _>>()?;
    record.lockfile = Some(ProductText::from_static("/project/Cargo.lock"));
    record.lockfile_membership = Some(ProjectLockfileMembership::Partial {
        unresolved: vec![ProjectUnresolvedLockMember {
            name: ProductText::from_static("unavailable"),
            version: Some(ProductText::from_static("1.0.0")),
            source: Some(ProductText::new("source\"\\line")?),
            reason: ProductText::from_static("unindexed source"),
        }]
        .into_boxed_slice(),
    });
    let encoded = serde_json::to_vec(&record)?;
    assert_eq!(serialized_json_size(&record), encoded.len());
    assert!(project_record_bound(&record) >= encoded.len());
    let reply = SurfaceReply::ProjectSynced(record);
    reply.admit(reply.id())?;
    assert!(reply.encoded_size_bound() >= serde_json::to_vec(&reply)?.len());
    Ok(())
}

#[test]
fn coverage_requires_a_selected_lockfile_source() -> Result<(), ProjectLockfileTestError> {
    let mut record = project()?;
    record.lockfile_membership = Some(ProjectLockfileMembership::Complete);
    assert_eq!(record.admit(), Err(ProductAdmissionError::RowBound));
    record.lockfile = Some(ProductText::from_static("/project/Cargo.lock"));
    record.admit()?;
    Ok(())
}
