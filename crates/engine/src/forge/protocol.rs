use super::*;

/// Remote delegation request pinned to one exact commit object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeDelegationRequest {
    /// Canonical coordinate identity without credentials.
    pub coordinate: ForgeCoordinate,
    /// Exact commit object that a remote worker must return.
    pub commit: ForgeObjectId,
}

impl ForgeDelegationRequest {
    /// Creates a request only after the caller has an exact commit resolution.
    pub fn new(coordinate: ForgeCoordinate) -> Result<Self, ForgeProtocolError> {
        let ForgeRevision::Commit(commit) = coordinate.revision().clone() else {
            return Err(ForgeProtocolError::ExactCommitRequired);
        };
        Ok(Self { coordinate, commit })
    }
}

/// Delegated exact source object returned by a remote worker.
#[derive(Debug)]
pub struct ForgeDelegatedObject {
    /// Exact commit object the worker claims to have served.
    pub commit: ForgeObjectId,
    /// Source archive selected by the worker.
    pub archive: ForgeArchive,
}

/// Typed forge protocol errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeProtocolError {
    /// A requested tag/branch was resolved to another commit than the exact
    /// commit request permitted.
    RevisionMismatch,
    /// Remote delegation must name an exact commit.
    ExactCommitRequired,
    /// Delegated response named a different commit.
    DelegatedCommitMismatch,
    /// A resolution was returned by a different repository authority.
    AuthorityMismatch,
    /// A mutable tag or branch did not include an observed validator.
    ValidatorRequired,
    /// A canonical field was malformed.
    Malformed,
}

/// Verifies a delegated exact commit claim before local archive admission.
pub fn verify_delegated_object(
    request: &ForgeDelegationRequest,
    response: ForgeDelegatedObject,
) -> Result<ForgeArchive, ForgeProtocolError> {
    if response.commit != request.commit {
        return Err(ForgeProtocolError::DelegatedCommitMismatch);
    }
    Ok(response.archive)
}
