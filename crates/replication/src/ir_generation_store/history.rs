//! Persistent commit ancestry and named navigation references for admitted
//! semantic generations. Commits point at the existing immutable generation
//! records; they never copy or lower the canonical semantic IR.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use backend_semantic::ir::{SemanticDeltaCursor, SemanticManifestError};
use backend_store::{ArtifactClosureClaim, ClosureId, ObjectId, UntrustedObjectId};

use super::{
    CHECKSUM_BYTES, GenerationRecord, LocalSemanticGeneration, LocalSemanticGenerationFiles,
    LocalSemanticGenerationId, Reader, SelectedGenerationSource, SelectedGenerationStamp,
    SemanticTargetKey, Writer, checked_body, create_private_directory, display_io,
    ensure_directory, ensure_optional_directory, ensure_regular_file, hex, load_record,
    read_optional_bounded, remove_file, require_current, set_private_directory,
    validate_record_selection,
};

#[cfg(test)]
use super::{HistoryTestFault, trip_history_test_fault};

// Protocol records and public value types stay in this facade; commit/ref
// mutation, replay, codecs, and collection run in focused child modules.

include!("history/types.rs");

mod catalog;
mod codec;
mod gc;
mod lineage;
mod provenance;
mod replay;
mod retention;
mod v2;
mod v3;

pub(super) use catalog::{
    may_prune_generation_records, read_history_catalog_snapshot, validate_commit_generation,
};
use codec::decode_history_index_intent;
pub(super) use codec::{
    append_history_index_entry, decode_history_segment_map_count, decode_history_segment_mapping,
    history_commit_path, history_index_intent_path, history_payload_root_path, load_history_commit,
    recover_history_index_intent, write_history_segment_map_count,
};
#[cfg(test)]
pub(super) use codec::{
    encode_history_payload_root_claim as encode_test_history_payload_root,
    encode_history_segment_mapping,
};
pub(super) use gc::{
    HistoryReachabilityClass, history_gc_epoch_root, history_gc_marked, history_index_id_at,
    read_history_gc_state,
};
pub(super) use provenance::decode_hex_digest;
pub(crate) use v2::{TypedV2HistoryLocator, TypedV2HistoryPublicationSnapshot};
pub(crate) use v3::TypedV3HistoryLocator;
pub use v3::{HistoryTypedV3LocatorId, HistoryTypedV3RootClaim};
pub use lineage::{
    BorrowedTypedLineageEdgeSetV1, LineageAttestationId, LineageAttestationVerifierV1,
    LineageConfirmationStatementV1,
    LineageCandidateGroupIdV1, LineageEdgeSetErrorV1, LineageEdgeV1, LineageEdgeViewV1,
    LineageHistoryEvidenceV1, LineageKindV1, LineageSourceV1, LineageStatusV1,
    LineageStatusViewV1, LineageEdgeIterV1, OwnedTypedLineageEdgeSetV1,
    RejectLineageConfirmationsV1, UnresolvedLineageReasonV1, UnprovenTypedLineageEdgeSetV1,
    VerifiedLineageEdgeIterV1, VerifiedLineageEdgeViewV1, VerifiedLineageStatusV1,
    VerifiedTypedLineageEdgeSetV1, VerifiedLineageRootV2,
    MAX_LINEAGE_CANDIDATES_PER_GROUP_V1, MAX_TYPED_LINEAGE_EDGES_V1,
    MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES,
};

pub(super) fn compact_history_tombstones(target_root: &Path) -> Result<(), String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok(());
    }
    let path = history_root.join("tombstones.index");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err("semantic history tombstone index is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(display_io(error)),
    }
    backend_platform::durable::write_private_atomic(&path, &[]).map_err(display_io)
}

pub(super) fn recover_pending_retention_delete(target_root: &Path) -> Result<(), String> {
    retention::recover_pending_delete(target_root)
}
