//! Defines publication format behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication format invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    mem::size_of,
    path::Path,
};

use blake3::Hasher;
use heart_hydration::VerifiedGenerationFacts;
use heart_identity::{ContentId, DependencySetDomain, GenerationId};
use server_workflow::StageKey;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U64},
};

use super::{
    errors::{
        ArtifactName, PublicationFailure, PublicationGenerationError, PublicationIoStep,
        PublicationOpenError,
    },
    facts::{
        ImmutablePublicationIdentity, PublicationFacts, PublicationHeadIdentity, PublicationPaths,
    },
    service::conflict,
};
use crate::{FrameSequence, JournalOffset, ReceiptFacts, format::CHECKSUM_BYTES};

const FACT_MAGIC: [u8; 8] = *b"NUDXPFC\0";
const HEAD_MAGIC: [u8; 8] = *b"NUDXPHD\0";
const PUBLICATION_VERSION: u16 = 1;
const FACT_DOMAIN: &[u8] = b"heart.publication.fact.v2\0";
const HEAD_DOMAIN: &[u8] = b"heart.publication.head.v2\0";
const KEY_DOMAIN: &[u8] = b"heart.publication.key.v1\0";
const OUTPUT_DOMAIN: &[u8] = b"heart.publication.output.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublicationInput {
    pub(super) root: [u8; 32],
    pub(super) dep_set: [u8; 32],
    pub(super) output: [u8; 32],
    pub(super) key: StageKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChainLink {
    pub(super) ordinal: u64,
    pub(super) parent_root: [u8; 32],
    pub(super) parent_dep_set: [u8; 32],
}

impl PublicationInput {
    pub(super) fn from_facts(facts: VerifiedGenerationFacts) -> Self {
        let root = *facts.pinned_root;
        let dep_set = *facts.dep_set;
        Self::from_parts(root, dep_set)
    }

    pub(super) fn from_parts(root: [u8; 32], dep_set: [u8; 32]) -> Self {
        let mut key_hasher = Hasher::new();
        key_hasher.update(KEY_DOMAIN);
        key_hasher.update(&root);
        key_hasher.update(&dep_set);
        let key = StageKey::from(*key_hasher.finalize().as_bytes());
        let mut output_hasher = Hasher::new();
        output_hasher.update(OUTPUT_DOMAIN);
        output_hasher.update(&root);
        output_hasher.update(&dep_set);
        let output = *output_hasher.finalize().as_bytes();
        Self {
            root,
            dep_set,
            output,
            key,
        }
    }

    pub(super) fn generation_facts(
        self,
    ) -> Result<VerifiedGenerationFacts, PublicationGenerationError> {
        let pinned_root =
            GenerationId::try_from(self.root).map_err(PublicationGenerationError::PinnedRoot)?;
        let dep_set = ContentId::<DependencySetDomain>::try_from(self.dep_set)
            .map_err(PublicationGenerationError::DependencySet)?;
        Ok(VerifiedGenerationFacts {
            pinned_root,
            dep_set,
        })
    }

    pub(super) fn matches_publication(self, publication: PublicationFacts) -> bool {
        self.root == *publication.generation.pinned_root
            && self.dep_set == *publication.generation.dep_set
            && head_identity(publication.immutable, publication.stable).checksum
                == publication.head.checksum
    }
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct FactRecord {
    magic: [u8; 8],
    version: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
    root: [u8; 32],
    dep_set: [u8; 32],
    output: [u8; 32],
    key: [u8; 32],
    ordinal: U64<LittleEndian>,
    parent_root: [u8; 32],
    parent_dep_set: [u8; 32],
    sequence: U64<LittleEndian>,
    durable_end: U64<LittleEndian>,
    checksum: [u8; CHECKSUM_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct HeadRecord {
    magic: [u8; 8],
    version: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
    fact_checksum: [u8; CHECKSUM_BYTES],
    sequence: U64<LittleEndian>,
    durable_end: U64<LittleEndian>,
    checksum: [u8; CHECKSUM_BYTES],
}

pub(super) const FACT_BYTES: usize = size_of::<FactRecord>();
const FACT_PAYLOAD_BYTES: usize = FACT_BYTES - CHECKSUM_BYTES;
pub(super) const HEAD_BYTES: usize = size_of::<HeadRecord>();
const HEAD_PAYLOAD_BYTES: usize = HEAD_BYTES - CHECKSUM_BYTES;

fn head_record(fact: ImmutablePublicationIdentity, receipt: ReceiptFacts) -> HeadRecord {
    let mut record = HeadRecord {
        magic: HEAD_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        fact_checksum: fact.checksum,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    record
}

fn head_identity(
    fact: ImmutablePublicationIdentity,
    receipt: ReceiptFacts,
) -> PublicationHeadIdentity {
    PublicationHeadIdentity {
        checksum: head_record(fact, receipt).checksum,
    }
}

pub(super) struct ParsedFact {
    pub(super) input: PublicationInput,
    pub(super) receipt: ReceiptFacts,
    pub(super) identity: ImmutablePublicationIdentity,
    pub(super) bytes: [u8; FACT_BYTES],
    pub(super) link: ChainLink,
}

pub(super) struct ParsedHead {
    pub(super) identity: PublicationHeadIdentity,
    pub(super) fact_checksum: [u8; CHECKSUM_BYTES],
    pub(super) receipt: ReceiptFacts,
    pub(super) bytes: [u8; HEAD_BYTES],
}

pub(super) fn path_exists(
    path: &Path,
    step: PublicationIoStep,
) -> Result<bool, PublicationOpenError> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PublicationOpenError::Io { step, source }),
    }
}

pub(super) fn read_fact(path: &Path) -> Result<Option<ParsedFact>, PublicationOpenError> {
    let Some(bytes) =
        read_fixed::<FACT_BYTES>(path, PublicationIoStep::ReadFact, ArtifactName::Fact)?
    else {
        return Ok(None);
    };
    parse_fact(bytes).map(Some)
}

pub(super) fn read_head(path: &Path) -> Result<Option<ParsedHead>, PublicationOpenError> {
    let Some(bytes) =
        read_fixed::<HEAD_BYTES>(path, PublicationIoStep::ReadHead, ArtifactName::Head)?
    else {
        return Ok(None);
    };
    parse_head(bytes).map(Some)
}

fn read_fixed<const BYTES: usize>(
    path: &Path,
    step: PublicationIoStep,
    artifact: ArtifactName,
) -> Result<Option<[u8; BYTES]>, PublicationOpenError> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PublicationOpenError::Io { step, source }),
    };
    let observed = file
        .metadata()
        .map_err(|source| PublicationOpenError::Io { step, source })?
        .len();
    if observed != BYTES as u64 {
        return Err(PublicationOpenError::Length {
            artifact,
            expected: BYTES,
            observed,
        });
    }
    let mut bytes = [0; BYTES];
    file.read_exact(&mut bytes)
        .map_err(|source| PublicationOpenError::Io { step, source })?;
    Ok(Some(bytes))
}

fn parse_fact(bytes: [u8; FACT_BYTES]) -> Result<ParsedFact, PublicationOpenError> {
    let record: FactRecord = zerocopy::transmute!(bytes);
    if record.magic != FACT_MAGIC
        || record.version.get() != PUBLICATION_VERSION
        || record.reserved.get() != 0
    {
        return Err(PublicationOpenError::EncodingMismatch);
    }
    if record.ordinal.get() == 0
        || (record.ordinal.get() == 1
            && (record.parent_root != [0; 32] || record.parent_dep_set != [0; 32]))
    {
        return Err(PublicationOpenError::EncodingMismatch);
    }
    let expected = checksum(FACT_DOMAIN, &record.as_bytes()[..FACT_PAYLOAD_BYTES]);
    if record.checksum != expected {
        return Err(PublicationOpenError::FactChecksum {
            expected,
            observed: record.checksum,
        });
    }
    Ok(ParsedFact {
        input: PublicationInput {
            root: record.root,
            dep_set: record.dep_set,
            output: record.output,
            key: StageKey::from(record.key),
        },
        receipt: ReceiptFacts {
            sequence: FrameSequence::from(record.sequence.get()),
            durable_end: JournalOffset::from(record.durable_end.get()),
        },
        identity: ImmutablePublicationIdentity {
            checksum: record.checksum,
        },
        bytes,
        link: ChainLink {
            ordinal: record.ordinal.get(),
            parent_root: record.parent_root,
            parent_dep_set: record.parent_dep_set,
        },
    })
}

fn parse_head(bytes: [u8; HEAD_BYTES]) -> Result<ParsedHead, PublicationOpenError> {
    let record: HeadRecord = zerocopy::transmute!(bytes);
    if record.magic != HEAD_MAGIC
        || record.version.get() != PUBLICATION_VERSION
        || record.reserved.get() != 0
    {
        return Err(PublicationOpenError::EncodingMismatch);
    }
    let expected = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    if record.checksum != expected {
        return Err(PublicationOpenError::HeadChecksum {
            expected,
            observed: record.checksum,
        });
    }
    Ok(ParsedHead {
        identity: PublicationHeadIdentity {
            checksum: record.checksum,
        },
        fact_checksum: record.fact_checksum,
        receipt: ReceiptFacts {
            sequence: FrameSequence::from(record.sequence.get()),
            durable_end: JournalOffset::from(record.durable_end.get()),
        },
        bytes,
    })
}

pub(super) struct PersistedFact {
    pub(super) identity: ImmutablePublicationIdentity,
}

pub(super) struct PersistedHead {
    pub(super) identity: PublicationHeadIdentity,
}

pub(super) fn persist_fact(
    paths: &PublicationPaths,
    input: PublicationInput,
    link: ChainLink,
    receipt: ReceiptFacts,
    bytes: &mut [u8; FACT_BYTES],
) -> Result<PersistedFact, PublicationFailure> {
    let mut record = FactRecord {
        magic: FACT_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        root: input.root,
        dep_set: input.dep_set,
        output: input.output,
        key: *input.key,
        ordinal: U64::new(link.ordinal),
        parent_root: link.parent_root,
        parent_dep_set: link.parent_dep_set,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(FACT_DOMAIN, &record.as_bytes()[..FACT_PAYLOAD_BYTES]);
    bytes.copy_from_slice(record.as_bytes());
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.fact_for(link.ordinal))
    {
        Ok(mut file) => {
            file.write_all(bytes)
                .map_err(|source| PublicationFailure::Io {
                    step: PublicationIoStep::WriteFact,
                    source,
                })?;
            file.sync_all().map_err(|source| PublicationFailure::Io {
                step: PublicationIoStep::SyncFact,
                source,
            })?;
            sync_parent_directory(
                &paths.fact(),
                PublicationIoStep::OpenFactParent,
                PublicationIoStep::SyncFactParent,
            )?;
            Ok(PersistedFact {
                identity: ImmutablePublicationIdentity {
                    checksum: record.checksum,
                },
            })
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let parsed =
                read_fact(&paths.fact_for(link.ordinal)).map_err(PublicationFailure::Open)?;
            let Some(parsed) = parsed else {
                return Err(PublicationFailure::Open(PublicationOpenError::MissingFact));
            };
            if parsed.bytes != *bytes {
                return Err(conflict(input, parsed.input));
            }
            Ok(PersistedFact {
                identity: parsed.identity,
            })
        }
        Err(source) => Err(PublicationFailure::Io {
            step: PublicationIoStep::CreateFact,
            source,
        }),
    }
}

pub(super) fn persist_head(
    paths: &PublicationPaths,
    fact: ImmutablePublicationIdentity,
    receipt: ReceiptFacts,
    bytes: &mut [u8; HEAD_BYTES],
) -> Result<PersistedHead, PublicationFailure> {
    let mut record = HeadRecord {
        magic: HEAD_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        fact_checksum: fact.checksum,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    bytes.copy_from_slice(record.as_bytes());
    match read_head(&paths.head()) {
        Ok(Some(existing)) => {
            if existing.bytes == *bytes {
                return Ok(PersistedHead {
                    identity: existing.identity,
                });
            }
        }
        Ok(None) => {}
        Err(error) => return Err(PublicationFailure::Open(error)),
    }
    if path_exists(&paths.head_temp(), PublicationIoStep::InspectHeadTemp)
        .map_err(PublicationFailure::Open)?
    {
        return Err(PublicationFailure::HeadTempPresent);
    }
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.head_temp())
        .map_err(|source| PublicationFailure::Io {
            step: PublicationIoStep::CreateHeadTemp,
            source,
        })?;
    temp.write_all(bytes)
        .map_err(|source| PublicationFailure::Io {
            step: PublicationIoStep::WriteHead,
            source,
        })?;
    temp.sync_all().map_err(|source| PublicationFailure::Io {
        step: PublicationIoStep::SyncHead,
        source,
    })?;
    fs::rename(paths.head_temp(), paths.head()).map_err(|source| PublicationFailure::Io {
        step: PublicationIoStep::RenameHead,
        source,
    })?;
    sync_parent_directory(
        &paths.head(),
        PublicationIoStep::OpenHeadParent,
        PublicationIoStep::SyncHeadParent,
    )?;
    Ok(PersistedHead {
        identity: PublicationHeadIdentity {
            checksum: record.checksum,
        },
    })
}

fn sync_parent_directory(
    path: &Path,
    open_step: PublicationIoStep,
    sync_step: PublicationIoStep,
) -> Result<(), PublicationFailure> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = File::open(parent).map_err(|source| PublicationFailure::Io {
        step: open_step,
        source,
    })?;
    directory
        .sync_all()
        .map_err(|source| PublicationFailure::Io {
            step: sync_step,
            source,
        })
}

fn checksum(domain: &[u8], bytes: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(domain);
    hasher.update(bytes);
    let mut output = [0; CHECKSUM_BYTES];
    output.copy_from_slice(&hasher.finalize().as_bytes()[..CHECKSUM_BYTES]);
    output
}
