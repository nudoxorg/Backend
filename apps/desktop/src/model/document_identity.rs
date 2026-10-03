//! Document and paint identity. These hashes separate native elements and
//! recent bounds; they never grant a read, owner attachment, or action lease.

use crate::model::pages::{CargoSourceKey, Stamp};
use backend_library::CargoPackageReadmeOriginV1;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DocumentIdentity([u8; 32]);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DocumentPaintIdentity(DocumentIdentity);

impl DocumentIdentity {
    /// Full origin, including binding, root scope, selection and byte digest.
    pub(crate) fn readme(origin: &CargoPackageReadmeOriginV1) -> Self {
        use backend_library::{
            CargoPackageReadmeRootScopeV1 as Scope, CargoPackageReadmeSelectionV1 as Selection,
        };
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox-cargo-readme-focus-v1");
        field(&mut hash, origin.package.as_str().as_bytes());
        hash.update(&origin.request_binding.schema.to_le_bytes());
        hash.update(&origin.request_binding.requested_root_digest);
        hash.update(&origin.request_binding.effective_workspace_root_digest);
        hash.update(&[match origin.root_scope {
            Scope::Package => 0,
            Scope::EffectiveWorkspace => 1,
        }]);
        field(&mut hash, origin.path.as_str().as_bytes());
        hash.update(&[match origin.selection {
            Selection::ManifestPath => 0,
            Selection::ManifestTrueDefault => 1,
            Selection::CargoConventionalDefault => 2,
            Selection::WorkspaceInherited => 3,
        }]);
        hash.update(&origin.content_digest);
        Self(*hash.finalize().as_bytes())
    }

    /// Hash the full typed selector, including the CargoSourceTarget variant.
    /// Equal display paths in package and README scopes are different files.
    pub(crate) fn cargo_source(key: &CargoSourceKey, content_digest: &[u8; 32]) -> Self {
        let mut writer = IdentityWriter::new(b"nudox-cargo-source-document-v1");
        key.hash(&mut writer);
        field(&mut writer.0, content_digest);
        Self(*writer.0.finalize().as_bytes())
    }

    /// A paint belongs to the actual Reader place and visible resource stamp.
    /// Same-root owner revocation changes that stamp before fresh bytes land.
    pub(crate) fn for_visit(self, place: u64, stamp: Stamp) -> DocumentPaintIdentity {
        let mut writer = IdentityWriter::new(b"nudox-document-paint-v1");
        field(&mut writer.0, &self.0);
        field(&mut writer.0, &place.to_le_bytes());
        stamp.hash(&mut writer);
        DocumentPaintIdentity(Self(*writer.0.finalize().as_bytes()))
    }

    pub(crate) fn endpoint(self, kind: &str, endpoint: &str) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(&self.0);
        field(&mut hash, kind.as_bytes());
        field(&mut hash, endpoint.as_bytes());
        hash.finalize()
    }
}

impl DocumentPaintIdentity {
    pub(crate) fn document_id(self) -> Arc<str> {
        self.element("document", 0)
    }
    pub(crate) fn heading_id(self, source_offset: usize) -> Arc<str> {
        self.element("heading", source_offset as u64)
    }
    pub(crate) fn line_id(self, line: u32) -> Arc<str> {
        self.element("line", u64::from(line))
    }
    pub(crate) fn pager_id(self) -> Arc<str> {
        self.element("pager", 0)
    }

    fn element(self, kind: &str, position: u64) -> Arc<str> {
        let mut hash = blake3::Hasher::new();
        field(&mut hash, &self.0.0);
        field(&mut hash, kind.as_bytes());
        field(&mut hash, &position.to_le_bytes());
        Arc::from(format!("document-{kind}-{}", hash.finalize().to_hex()))
    }
}

fn field(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

struct IdentityWriter(blake3::Hasher);
impl IdentityWriter {
    fn new(domain: &[u8]) -> Self {
        let mut hash = blake3::Hasher::new();
        field(&mut hash, domain);
        Self(hash)
    }
}
impl Hasher for IdentityWriter {
    fn finish(&self) -> u64 {
        let digest = self.0.finalize();
        let mut word = [0; 8];
        word.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(word)
    }
    fn write(&mut self, bytes: &[u8]) {
        field(&mut self.0, bytes);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::pages::{Capacity, PageKey, PageStore};
    use crate::navigation::{CargoReadmeLinkAddress, CargoSourcePath, CargoSourceTarget};

    #[test]
    fn exact_target_and_content_separate_equal_display_paths() {
        let (_, selected, result) = crate::runtime::cargo_readme_reads::tests::fixture();
        let origin = CargoPackageReadmeOriginV1::from_result(&result).expect("shape-valid origin");
        let link = CargoReadmeLinkAddress::new(origin, "../src/lib.rs#L7")
            .expect("workspace-relative target");
        let package = CargoSourceKey {
            context: selected.context.clone(),
            package: selected.package.clone(),
            target: CargoSourceTarget::PackageFile(
                CargoSourcePath::new("src/lib.rs").expect("package-relative path"),
            ),
        };
        let workspace = CargoSourceKey {
            target: CargoSourceTarget::ReadmeLink(link),
            ..package.clone()
        };
        assert_eq!(package.target.path(), workspace.target.path());
        let digest = *blake3::hash(b"pub fn same_bytes() {}\n").as_bytes();
        let package_id = DocumentIdentity::cargo_source(&package, &digest);
        let workspace_id = DocumentIdentity::cargo_source(&workspace, &digest);
        assert_ne!(
            package_id, workspace_id,
            "the target variant and complete README origin are part of document identity"
        );
        let changed = *blake3::hash(b"pub fn different_bytes() {}\n").as_bytes();
        assert_ne!(
            package_id,
            DocumentIdentity::cargo_source(&package, &changed)
        );
        assert_eq!(
            package_id,
            DocumentIdentity::cargo_source(&package, &digest)
        );
    }

    #[test]
    fn current_place_and_visible_slot_stamp_separate_recent_bounds() {
        let (_, selected, _) = crate::runtime::cargo_readme_reads::tests::fixture();
        let key = CargoSourceKey {
            context: selected.context,
            package: selected.package,
            target: CargoSourceTarget::PackageFile(
                CargoSourcePath::new("src/lib.rs").expect("path"),
            ),
        };
        let document = DocumentIdentity::cargo_source(&key, &[7; 32]);
        let key = PageKey::CargoSource(key);
        let mut pages = PageStore::new(Capacity::default());
        let before = pages.stamp(&key);
        let first = document.for_visit(1, before);
        assert_eq!(first, document.for_visit(1, before));
        let returning = document.for_visit(2, before);
        assert_ne!(first.document_id(), returning.document_id());
        assert_ne!(first.line_id(7), returning.line_id(7));
        assert_ne!(first.heading_id(0), returning.heading_id(0));
        assert_ne!(first.pager_id(), returning.pager_id());
        // UI identity only: an unserved slot does not establish owner/read
        // acceptance. A real owner-replacement test lives in store tests.
        pages
            .begin(&key, VersionedRoot::unserved())
            .expect("pending slot");
        let pending = pages.stamp(&key);
        assert_ne!(before, pending);
        assert_ne!(first, document.for_visit(1, pending));
        pages.cancel(&key).expect("cancel pending generation");
        assert_ne!(
            document.for_visit(1, pending),
            document.for_visit(1, pages.stamp(&key))
        );
    }
}
