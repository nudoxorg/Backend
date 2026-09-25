//! A later upsert that did not observe a digest keeps the one already stored.
//! A git SHA-1 does not replace a registry SHA-256 or a BLAKE3.

use turso_versioning::orm::OrmResult;

use crate::{pid::ContentDigest, record::PackageRecord};

use super::turso_vc::VersionedCatalog;

impl VersionedCatalog {
    pub(super) fn retain_known_content(
        &mut self,
        record: &PackageRecord,
    ) -> OrmResult<PackageRecord> {
        let prior = self
            .get(
                record.ecosystem.as_token(),
                record.canonical_name.as_str(),
                record.version.as_str(),
            )
            .map(|fact| fact.to_record())
            .transpose()?
            .and_then(|prior| prior.content);
        let mut kept = record.clone();
        kept.content = merge_content(prior, record.content);
        Ok(kept)
    }
}

fn merge_content(
    prior: Option<ContentDigest>,
    observed: Option<ContentDigest>,
) -> Option<ContentDigest> {
    match observed {
        Some(ContentDigest::Sha256(bytes)) => Some(ContentDigest::Sha256(bytes)),
        Some(ContentDigest::Blake3(bytes)) => Some(ContentDigest::Blake3(bytes)),
        Some(ContentDigest::GitSha1(_))
            if matches!(
                prior,
                Some(ContentDigest::Sha256(_)) | Some(ContentDigest::Blake3(_))
            ) =>
        {
            prior
        }
        Some(observed) => Some(observed),
        None => prior,
    }
}
