//! Canonical Merkle page proof admission.

use super::{
    BuiltinAdmission, IdContext, ImmutableObjectSchema, ObjectKey, ObjectVersion, ProductRelation,
    Relation, WorkerError, complete_coverage, product_source_row_bytes,
    schema_object_version_identity_claim,
};

impl BuiltinAdmission {
    #[allow(clippy::too_many_lines)]
    pub(super) fn admit_page_proof(
        &mut self,
        response: &backend_engine::ClosurePageResponse,
    ) -> Result<(), WorkerError> {
        let page = &response.page;
        let proof = response.proof.as_slice();
        if page.node == page.root.digest() {
            // A page's root is still a peer supplied claim. Recompute the
            // exact synthetic closure identity from the canonical manifest
            // proof and admit the claim against that typed root before using
            // any child.
            let expected_root = backend_engine::MerkleRoot::from_admitted_manifest(
                1,
                ObjectVersion::<ImmutableObjectSchema>::from_value(proof),
            );
            page.root
                .to_claim()
                .admit_against(expected_root)
                .map_err(|_| WorkerError::InputMismatch)?;
            let claim = schema_object_version_identity_claim::<ImmutableObjectSchema>(&page.node.0)
                .map_err(|_| WorkerError::InputMismatch)?;
            let claim = backend_engine::admit_product_closure_manifest(claim, proof)
                .map_err(|_| WorkerError::InputMismatch)?;
            // The offer carries the canonical workspace manifest, while this
            // page proves the synthetic execution closure. They are distinct
            // authenticated objects. Their join is the typed workspace root
            // plus the relation child committed by both; comparing their
            // byte encodings would reject every valid production closure.
            if self.workspace_manifest.is_none() {
                return Err(WorkerError::InputMismatch);
            }
            let workspace = claim.workspace;
            let relation = claim.relation;
            let input = claim.input;
            let Some((_, expected_workspace, _)) = self.pending_root else {
                return Err(WorkerError::InputMismatch);
            };
            if workspace != expected_workspace {
                return Err(WorkerError::InputMismatch);
            }
            self.manifest_relation = Some(relation);
            let backend_engine::MerklePageBody::Branch(children) = &page.body else {
                return Err(WorkerError::InputMismatch);
            };
            if children.len() != 1 || children[0].digest.as_bytes() != relation {
                return Err(WorkerError::InputMismatch);
            }
            // ProductInput is the relation-root bytes. The manifest commits
            // both identities, so derive the dedicated input claim here
            // instead of adding a mixed-height pseudo-leaf to the Merkle
            // branch. This keeps page shape canonical at every tree level.
            let input_bytes = relation;
            let input_version = ObjectVersion::<ImmutableObjectSchema>::from_value(&input_bytes);
            if input_version.to_bytes() != input {
                return Err(WorkerError::InputMismatch);
            }
            let input_claim = schema_object_version_identity_claim::<ImmutableObjectSchema>(&input)
                .map_err(|_| WorkerError::InputMismatch)?;
            self.input_cas
                .retain_claim(page.root, input_claim)
                .map_err(WorkerError::Replication)?;
            self.input_cas
                .record_input_claim(page.root, input_claim)
                .map_err(WorkerError::Replication)?;
            if !self.input_cas.contains_claim(input_claim) {
                self.input_cas
                    .append_missing(page.root, input_claim)
                    .map_err(WorkerError::Replication)?;
            }
            self.input_cas
                .record_node_proof(page.node.0, proof)
                .map_err(WorkerError::Replication)?;
            self.input_cas
                .record_node(page.node.0)
                .map_err(WorkerError::Replication)?;
            return Ok(());
        }
        let Some(relation) = self.manifest_relation else {
            return Err(WorkerError::InputMismatch);
        };
        // Every descendant request was minted from a child commitment in a
        // previously admitted parent page. Re-admit this node independently
        // against that exact digest; no raw page body can manufacture a
        // checked relation node.
        let claim = backend_engine::UntrustedId::<ProductRelation>::from_wire(
            &page.node.0,
            IdContext::relation::<ProductRelation>(),
        )
        .map_err(|_| WorkerError::InputProof("relation identity"))?;
        let checked = backend_engine::admit_canonical_root_claim::<ProductRelation>(claim, proof)
            .map_err(|_| WorkerError::InputProof("canonical relation root"))?;
        if page.node.0 == relation {
            let manifest = self
                .workspace_manifest
                .as_ref()
                .ok_or(WorkerError::InputProof("workspace manifest presence"))?;
            if manifest.relations()[0].root() != page.node.0 {
                return Err(WorkerError::InputProof("workspace relation binding"));
            }
            let admitted = self
                .pending_authority
                .ok_or(WorkerError::InputProof("pending authority"))?;
            let coverage = complete_coverage(self.authority, admitted)
                .map_err(|_| WorkerError::InputProof("authority coverage"))?;
            let relation_binding = backend_engine::RelationBinding::from_persisted_root(
                &backend_engine::PersistedTreeRoot::from_checked(checked.clone()),
                coverage,
            );
            let admitted = manifest
                .clone()
                .admit_checked(
                    vec![relation_binding],
                    Vec::new(),
                    backend_engine::ObjectClosure::from_version(self.authority),
                    coverage,
                )
                .map_err(WorkerError::Workspace)?;
            let Some((_, expected_workspace, _)) = self.pending_root else {
                return Err(WorkerError::InputProof("pending workspace root"));
            };
            if admitted.root().as_bytes() != &expected_workspace {
                return Err(WorkerError::InputProof("admitted workspace root"));
            }
            self.admitted_workspace_manifest = Some(admitted);
            self.relation_proof = Some(proof.to_vec());
        }
        if checked.node().level() != page.level {
            return Err(WorkerError::InputProof("relation page level"));
        }
        let offset = usize::try_from(page.cursor.offset)
            .map_err(|_| WorkerError::InputProof("relation page cursor"))?;
        match &page.body {
            backend_engine::MerklePageBody::Branch(children) => {
                let mut iter = checked
                    .child_iter()
                    .map_err(|_| WorkerError::InputProof("relation branch iterator"))?;
                if offset > 0 {
                    let _ = iter
                        .nth(offset.saturating_sub(1))
                        .ok_or(WorkerError::InputProof("relation branch offset"))?;
                }
                for child in children {
                    let expected = iter
                        .next()
                        .ok_or(WorkerError::InputProof("relation branch child"))?;
                    let expected =
                        expected.map_err(|_| WorkerError::InputProof("relation branch proof"))?;
                    let key = {
                        let mut bytes = Vec::new();
                        ProductRelation::encode_key(&expected.first_key, &mut bytes);
                        bytes
                    };
                    if key != child.first_key
                        || expected.commitment.as_bytes() != &child.digest.as_bytes()
                        || expected.level != child.level
                    {
                        return Err(WorkerError::InputProof("relation branch body"));
                    }
                }
            }
            backend_engine::MerklePageBody::Leaf(entries) => {
                let fields = checked
                    .leaf_entries()
                    .map_err(|_| WorkerError::InputProof("relation leaf entries"))?;
                for (index, entry) in entries.iter().enumerate() {
                    let (key, value) = fields
                        .get(offset.saturating_add(index))
                        .ok_or(WorkerError::InputProof("relation leaf offset"))?;
                    let mut key_bytes = Vec::new();
                    key_bytes.extend_from_slice(key);
                    let row = product_source_row_bytes(key, value);
                    let version = ObjectVersion::<ImmutableObjectSchema>::from_value(&row);
                    let key_id = ObjectKey::<ImmutableObjectSchema>::from_value(&row);
                    if entry.key != key_bytes
                        || entry.key_id != key_id.to_bytes()
                        || entry.version != version.to_bytes()
                        || entry.len != row.len() as u64
                    {
                        return Err(WorkerError::InputProof("relation leaf body"));
                    }
                }
            }
        }
        self.input_cas
            .record_node_proof(page.node.0, proof)
            .map_err(WorkerError::Replication)?;
        self.input_cas
            .record_node(page.node.0)
            .map_err(WorkerError::Replication)?;
        Ok(())
    }
}
