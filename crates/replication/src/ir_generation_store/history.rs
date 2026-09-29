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
use backend_store::{ClosureId, ObjectId};

use super::{
    GenerationRecord, LocalSemanticGeneration, LocalSemanticGenerationFiles,
    LocalSemanticGenerationId, Reader, SelectedGenerationSource, SelectedGenerationStamp,
    SemanticTargetKey, Writer, checked_body, create_private_directory, display_io,
    ensure_directory, ensure_optional_directory, ensure_regular_file, hex, load_record,
    read_optional_bounded, remove_file, require_current, set_private_directory,
    validate_record_selection,
};

// Protocol records and public value types stay in this facade; commit/ref
// mutation, replay, codecs, and collection run in focused child modules.

include!("history/types.rs");

mod catalog;
mod codec;
mod gc;
mod provenance;
mod replay;

pub(super) use catalog::may_prune_generation_records;
