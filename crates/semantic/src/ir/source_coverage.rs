//! Compiler-observed coverage facts. These explain omissions without granting completeness.
use backend_version::{ContentId, Schema, SourceFactDomain};
use serde::{Deserialize, Serialize};
use std::mem::size_of;

/// Independent finite observation-row recording allowance; never a semantic source limit.
pub const MAX_SOURCE_COVERAGE_ROWS: usize = 16_384 + 256;
/// Bound independently of transport paging; the catalog lives once in CAS.
pub const MAX_SOURCE_COVERAGE_BYTES: usize = 8 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"BKSCOV01";

/// Exact immutable coverage catalog schema.
pub struct SourceCoverageSchema;
impl Schema for SourceCoverageSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00a;
    const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

fn validate_path_syntax(path: &str) -> Result<(), &'static str> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err("invalid coverage source path");
    }
    Ok(())
}

/// A selected native configuration or one compiler-observed source omission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceCoverageRow {
    /// Config selected by the actual project producer; never inferred from file names.
    Configuration {
        /// Captured workspace-relative config path.
        path: String,
        /// Exact config source-fact identity.
        source: [u8; 32],
        /// Native program options and compiler-read environment identity.
        environment: [u8; 32],
    },
    /// Captured input which the compiler did not publish as a semantic artifact.
    Gap {
        /// Exact package-relative path.
        path: String,
        /// Exact input source-fact identity.
        source: [u8; 32],
        /// Exact captured source extent.
        byte_len: u32,
        /// Producer-observed cause, distinct from a terminal profile failure.
        cause: SourceCoverageGapCause,
        /// Exact TypeScript owner context for a source-local type-projection cycle.
        /// Its offsets are into the source identity and byte length above; it
        /// deliberately names the owner declaration, not the recursive edge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostic: Option<SourceCoverageGapDiagnostic>,
    },
}

/// Bounded source context retained for one exact TSZ type-projection cycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceCoverageGapDiagnostic {
    /// A repeated native TSZ type handle while projecting the named owner.
    TypeProjectionCycle {
        /// Opaque TSZ handle payload; this is diagnostic identity, not a type name.
        type_id: u32,
        /// Exact owner declaration byte range in the captured source.
        owner_start: u32,
        /// Exclusive end of the owner declaration byte range.
        owner_end: u32,
        /// Exact owner identifier byte range in the captured source.
        owner_name_start: u32,
        /// Exclusive end of the owner identifier byte range.
        owner_name_end: u32,
        /// One-based line containing the owner declaration.
        line: u32,
        /// One-based Unicode-scalar column at the start of the owner declaration.
        column: u32,
        /// Exact identifier spelling sliced from the captured source.
        owner_name: String,
    },
}

impl SourceCoverageGapDiagnostic {
    fn validate(&self, byte_len: u32) -> Result<(), &'static str> {
        match self {
            Self::TypeProjectionCycle {
                owner_start,
                owner_end,
                owner_name_start,
                owner_name_end,
                line,
                column,
                owner_name,
                ..
            } if owner_start < owner_end
                && owner_end <= &byte_len
                && owner_start <= owner_name_start
                && owner_name_start < owner_name_end
                && owner_name_end <= owner_end
                && *line > 0
                && *column > 0
                && !owner_name.is_empty()
                && owner_name.len() <= 256
                && owner_name_end - owner_name_start == owner_name.len() as u32
                && !owner_name.chars().any(char::is_control) => Ok(()),
            _ => Err("invalid TypeScript coverage diagnostic"),
        }
    }

    /// Human-readable context that preserves the distinction between owner and cycle edge.
    pub fn summary(&self) -> String {
        match self {
            Self::TypeProjectionCycle {
                type_id,
                line,
                column,
                owner_start,
                owner_end,
                owner_name,
                ..
            } => format!(
                "owner declaration {owner_name:?} at {line}:{column} (bytes {owner_start}..{owner_end}); TSZ handle {type_id}"
            ),
        }
    }
}
impl SourceCoverageRow {
    /// Exact normalized path, in the row's declared coordinate space.
    pub fn path(&self) -> &str {
        match self {
            Self::Configuration { path, .. } | Self::Gap { path, .. } => path,
        }
    }
    /// This catalog belongs only to native TypeScript. Other language causes
    /// remain available to their engine producers but cannot enter this carrier.
    pub fn validate_typescript(&self) -> Result<(), &'static str> {
        self.validate()?;
        if matches!(
            self,
            Self::Gap {
                cause: SourceCoverageGapCause::RustSourceOutsideActiveCargoTarget
                    | SourceCoverageGapCause::PythonSourceSyntaxUnavailable
                    | SourceCoverageGapCause::PythonSourceEncodingUnavailable
                    | SourceCoverageGapCause::PythonSourceDependencyUnavailable,
                ..
            }
        ) {
            return Err("coverage cause belongs to another language");
        }
        Ok(())
    }
    fn key(&self) -> (u8, &str) {
        (u8::from(matches!(self, Self::Gap { .. })), self.path())
    }
    /// Validates bounded canonical source coordinates, including their typed source domain.
    pub fn validate(&self) -> Result<(), &'static str> {
        let path = self.path();
        if path.is_empty()
            || path.len() > 4096
            || path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err("invalid coverage source path");
        }
        let source = match self {
            Self::Configuration { source, .. } | Self::Gap { source, .. } => source,
        };
        ContentId::<SourceFactDomain>::try_from(*source)
            .map_err(|_| "invalid coverage source identity")?;
        if matches!(self, Self::Configuration{environment,..} if *environment==[0;32]) {
            return Err("invalid coverage config environment");
        }
        if let Self::Gap {
            byte_len,
            cause,
            diagnostic: Some(diagnostic),
            ..
        } = self
        {
            if *cause != SourceCoverageGapCause::SourceNotRepresentable {
                return Err("coverage diagnostic requires a source representation gap");
            }
            diagnostic.validate(*byte_len)?;
        }
        Ok(())
    }
}

/// Canonical immutable, source-bound observations from one compiler invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceCoverageManifest {
    identity: [u8; 32],
    encoded_len: usize,
    input_root: [u8; 32],
    profile: [u8; 2],
    program: [u8; 32],
    source_count: u32,
    completed: u32,
    rows: Vec<SourceCoverageRow>,
}
impl SourceCoverageManifest {
    /// Admits producer facts. Config rows precede gap rows, each in strict path order.
    pub fn new(
        input_root: [u8; 32],
        profile: [u8; 2],
        program: [u8; 32],
        source_count: u32,
        completed: u32,
        rows: Vec<SourceCoverageRow>,
    ) -> Result<Self, &'static str> {
        if program == [0; 32]
            || input_root == [0; 32]
            || !matches!(
                crate::vocabulary::LanguageProfile::try_from(profile),
                Ok(crate::vocabulary::LanguageProfile::TypeScript(_))
            )
            || source_count == 0
            || source_count > 100_000
            || completed > source_count
            || rows.len() > MAX_SOURCE_COVERAGE_ROWS
        {
            return Err("invalid coverage basis");
        }
        let mut gaps = 0usize;
        for row in &rows {
            row.validate_typescript()?;
            if matches!(row, SourceCoverageRow::Gap { .. }) {
                gaps += 1;
            }
        }
        if rows.len() - gaps > 256
            || rows.windows(2).any(|w| w[0].key() >= w[1].key())
            || gaps != (source_count - completed) as usize
        {
            return Err("invalid coverage partition");
        }
        let mut value = Self {
            identity: [0; 32],
            encoded_len: 0,
            input_root,
            profile,
            program,
            source_count,
            completed,
            rows,
        };
        let length = value
            .rows
            .iter()
            .try_fold(86usize, |length, row| {
                length
                    .checked_add(row.path().len())?
                    .checked_add(match row {
                        SourceCoverageRow::Configuration { .. } => 69,
                        SourceCoverageRow::Gap { diagnostic, .. } => {
                            42 + diagnostic.as_ref().map_or(0, |diagnostic| match diagnostic {
                                SourceCoverageGapDiagnostic::TypeProjectionCycle {
                                    owner_name, ..
                                } => {
                                    32 + owner_name.len()
                                }
                            })
                        }
                    })
            })
            .ok_or("coverage catalog length overflow")?;
        if length > MAX_SOURCE_COVERAGE_BYTES {
            return Err("coverage catalog capacity exceeded");
        }
        value.encoded_len = length;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.compiler-source-coverage.v1\0");
        hasher.update(&value.encode());
        value.identity = *hasher.finalize().as_bytes();
        Ok(value)
    }
    /// Compiler input witness, with no completeness upgrade.
    pub const fn input_root(&self) -> &[u8; 32] {
        &self.input_root
    }
    /// Exact language profile.
    pub const fn profile(&self) -> [u8; 2] {
        self.profile
    }
    /// Exact native program/forest identity authenticated with these observations.
    pub const fn program(&self) -> &[u8; 32] {
        &self.program
    }
    /// Captured source count.
    pub const fn source_count(&self) -> u32 {
        self.source_count
    }
    /// Published semantic artifact count.
    pub const fn completed(&self) -> u32 {
        self.completed
    }
    /// Canonical config and gap facts.
    pub fn rows(&self) -> &[SourceCoverageRow] {
        &self.rows
    }
    /// Stable domain-separated catalog identity; not the outer CAS object identity.
    pub const fn identity(&self) -> [u8; 32] {
        self.identity
    }
    /// Exact payload length, computed before allocating canonical bytes.
    pub const fn encoded_len(&self) -> usize {
        self.encoded_len
    }
    /// Exact owned row/vector/path storage charged by immutable residence.
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.rows.capacity() * size_of::<SourceCoverageRow>()
            + self
                .rows
                .iter()
                .map(|row| match row {
                    SourceCoverageRow::Configuration { path, .. }
                    | SourceCoverageRow::Gap { path, .. } => path.capacity(),
                })
                .sum::<usize>()
            + self
                .rows
                .iter()
                .map(|row| match row {
                    SourceCoverageRow::Gap {
                        diagnostic:
                            Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
                                owner_name,
                                ..
                            }),
                        ..
                    } => owner_name.capacity(),
                    _ => 0,
                })
                .sum::<usize>()
    }
    /// Exact canonical typed-object payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.input_root);
        out.extend_from_slice(&self.profile);
        out.extend_from_slice(&self.program);
        out.extend_from_slice(&self.source_count.to_le_bytes());
        out.extend_from_slice(&self.completed.to_le_bytes());
        out.extend_from_slice(&(self.rows.len() as u32).to_le_bytes());
        for row in &self.rows {
            let row_tag = match row {
                SourceCoverageRow::Gap {
                    diagnostic: Some(_),
                    ..
                } => 2,
                _ => row.key().0,
            };
            out.push(row_tag);
            out.extend_from_slice(&(row.path().len() as u32).to_le_bytes());
            out.extend_from_slice(row.path().as_bytes());
            match row {
                SourceCoverageRow::Configuration {
                    source,
                    environment,
                    ..
                } => {
                    out.extend_from_slice(source);
                    out.extend_from_slice(environment);
                }
                SourceCoverageRow::Gap {
                    source,
                    byte_len,
                    cause,
                    diagnostic,
                } => {
                    out.extend_from_slice(source);
                    out.extend_from_slice(&byte_len.to_le_bytes());
                    out.push(cause.tag());
                    if let Some(diagnostic) = diagnostic {
                        match diagnostic {
                            SourceCoverageGapDiagnostic::TypeProjectionCycle {
                                type_id,
                                owner_start,
                                owner_end,
                                owner_name_start,
                                owner_name_end,
                                line,
                                column,
                                owner_name,
                            } => {
                                out.extend_from_slice(&type_id.to_le_bytes());
                                out.extend_from_slice(&owner_start.to_le_bytes());
                                out.extend_from_slice(&owner_end.to_le_bytes());
                                out.extend_from_slice(&owner_name_start.to_le_bytes());
                                out.extend_from_slice(&owner_name_end.to_le_bytes());
                                out.extend_from_slice(&line.to_le_bytes());
                                out.extend_from_slice(&column.to_le_bytes());
                                out.extend_from_slice(&(owner_name.len() as u32).to_le_bytes());
                                out.extend_from_slice(owner_name.as_bytes());
                            }
                        }
                    }
                }
            }
        }
        out
    }
    /// Rejects malformed, oversized, reordered, duplicate and trailing facts.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_SOURCE_COVERAGE_BYTES {
            return Err("coverage catalog capacity exceeded");
        }
        let mut d = Decoder(bytes);
        if d.take(8)? != MAGIC {
            return Err("invalid coverage magic");
        }
        let root = d.array()?;
        let profile = d.array()?;
        let program = d.array()?;
        let count = d.u32()?;
        let completed = d.u32()?;
        let rows = d.u32()? as usize;
        if rows > MAX_SOURCE_COVERAGE_ROWS {
            return Err("coverage row capacity exceeded");
        }
        let mut facts = Vec::with_capacity(rows);
        for _ in 0..rows {
            let tag = d.take(1)?[0];
            let len = d.u32()? as usize;
            if len > 4096 {
                return Err("coverage path capacity exceeded");
            }
            let path = std::str::from_utf8(d.take(len)?)
                .map_err(|_| "coverage path encoding")?
                .to_owned();
            let source = d.array()?;
            facts.push(match tag {
                0 => SourceCoverageRow::Configuration {
                    path,
                    source,
                    environment: d.array()?,
                },
                1 | 2 => SourceCoverageRow::Gap {
                    path,
                    source,
                    byte_len: d.u32()?,
                    cause: SourceCoverageGapCause::from_tag(d.take(1)?[0])?,
                    diagnostic: if tag == 2 {
                        let type_id = d.u32()?;
                        let owner_start = d.u32()?;
                        let owner_end = d.u32()?;
                        let owner_name_start = d.u32()?;
                        let owner_name_end = d.u32()?;
                        let line = d.u32()?;
                        let column = d.u32()?;
                        let name_len = d.u32()? as usize;
                        if name_len > 256 {
                            return Err("coverage diagnostic name capacity exceeded");
                        }
                        let owner_name = std::str::from_utf8(d.take(name_len)?)
                            .map_err(|_| "coverage diagnostic name encoding")?
                            .to_owned();
                        Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
                            type_id,
                            owner_start,
                            owner_end,
                            owner_name_start,
                            owner_name_end,
                            line,
                            column,
                            owner_name,
                        })
                    } else {
                        None
                    },
                },
                _ => return Err("invalid coverage row tag"),
            });
        }
        if !d.0.is_empty() {
            return Err("trailing coverage bytes");
        }
        Self::new(root, profile, program, count, completed, facts)
    }
}

/// The complete explanation could not fit its independent recording budget.
/// This never changes the semantic compiler's source or publication outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCoverageCapacityReason {
    /// Too many complete observation rows for this catalog representation.
    RowCount,
    /// An otherwise admitted source/config path exceeds the explanation row bound.
    PathBytes,
    /// The complete canonical explanation exceeds its recording byte allowance.
    EncodedBytes,
}
impl SourceCoverageCapacityReason {
    fn tag(self) -> u8 {
        match self {
            Self::RowCount => 1,
            Self::PathBytes => 2,
            Self::EncodedBytes => 3,
        }
    }
    fn from_tag(tag: u8) -> Result<Self, &'static str> {
        match tag {
            1 => Ok(Self::RowCount),
            2 => Ok(Self::PathBytes),
            3 => Ok(Self::EncodedBytes),
            _ => Err("invalid coverage capacity reason"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Observation {
    Catalog(SourceCoverageManifest),
    NotRecordedCapacity {
        input: [u8; 32],
        profile: [u8; 2],
        program: [u8; 32],
        source_count: u32,
        completed: u32,
        reason: SourceCoverageCapacityReason,
    },
}

/// Authenticated all-or-unavailable coverage recording. Capacity is explicit,
/// never an incomplete catalog or a compiler failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceCoverageObservation {
    observation: Observation,
}
const OBSERVATION_MAGIC: &[u8; 8] = b"BKSCOB01";
/// Exact wrapper cost used by producer forecasting before copying any rows.
pub const SOURCE_COVERAGE_OBSERVATION_OVERHEAD: usize = 9;
impl SourceCoverageObservation {
    /// Exact all-row capacity forecast over borrowed paths. Syntax faults are
    /// errors even when volume is also excessive; they never become capacity.
    pub fn forecast<'a>(
        configs: impl IntoIterator<Item = &'a str>,
        gaps: impl IntoIterator<Item = &'a str>,
    ) -> Result<Option<SourceCoverageCapacityReason>, &'static str> {
        let mut rows = 0usize;
        let mut bytes = Some(86usize + SOURCE_COVERAGE_OBSERVATION_OVERHEAD);
        let mut long_path = false;
        for (path, cost) in configs
            .into_iter()
            .map(|path| (path, 69usize))
            .chain(gaps.into_iter().map(|path| (path, 42usize)))
        {
            validate_path_syntax(path)?;
            rows = rows.checked_add(1).ok_or("coverage row count overflow")?;
            long_path |= path.len() > 4096;
            bytes = bytes
                .and_then(|bytes| bytes.checked_add(cost))
                .and_then(|bytes| bytes.checked_add(path.len()));
        }
        Ok(if rows > MAX_SOURCE_COVERAGE_ROWS {
            Some(SourceCoverageCapacityReason::RowCount)
        } else if long_path {
            Some(SourceCoverageCapacityReason::PathBytes)
        } else if bytes.is_none_or(|bytes| bytes > MAX_SOURCE_COVERAGE_BYTES) {
            Some(SourceCoverageCapacityReason::EncodedBytes)
        } else {
            None
        })
    }
    /// Wraps a complete admitted catalog; never drops rows to meet a bound.
    pub fn from_catalog(catalog: SourceCoverageManifest) -> Result<Self, &'static str> {
        if catalog
            .encoded_len()
            .checked_add(SOURCE_COVERAGE_OBSERVATION_OVERHEAD)
            .is_none_or(|len| len > MAX_SOURCE_COVERAGE_BYTES)
        {
            return Err("coverage recording capacity exceeded");
        }
        Ok(Self {
            observation: Observation::Catalog(catalog),
        })
    }
    /// Records capacity unavailability bound to the actual successful compiler basis.
    pub fn not_recorded_capacity(
        input: [u8; 32],
        profile: [u8; 2],
        program: [u8; 32],
        source_count: u32,
        completed: u32,
        reason: SourceCoverageCapacityReason,
    ) -> Result<Self, &'static str> {
        if input == [0; 32]
            || program == [0; 32]
            || !matches!(
                crate::vocabulary::LanguageProfile::try_from(profile),
                Ok(crate::vocabulary::LanguageProfile::TypeScript(_))
            )
            || source_count == 0
            || source_count > 100_000
            || completed > source_count
        {
            return Err("invalid coverage capacity basis");
        }
        Ok(Self {
            observation: Observation::NotRecordedCapacity {
                input,
                profile,
                program,
                source_count,
                completed,
                reason,
            },
        })
    }
    /// Whole catalog, absent when no cause rows were recorded.
    pub fn catalog(&self) -> Option<&SourceCoverageManifest> {
        match &self.observation {
            Observation::Catalog(catalog) => Some(catalog),
            _ => None,
        }
    }
    /// Exact typed reason the catalog was not recorded.
    pub const fn capacity_reason(&self) -> Option<SourceCoverageCapacityReason> {
        match &self.observation {
            Observation::NotRecordedCapacity { reason, .. } => Some(*reason),
            _ => None,
        }
    }
    /// Exact compiler input; not a completeness capability.
    pub const fn input_root(&self) -> &[u8; 32] {
        match &self.observation {
            Observation::Catalog(catalog) => catalog.input_root(),
            Observation::NotRecordedCapacity { input, .. } => input,
        }
    }
    /// Exact compiler language profile.
    pub const fn profile(&self) -> [u8; 2] {
        match &self.observation {
            Observation::Catalog(catalog) => catalog.profile(),
            Observation::NotRecordedCapacity { profile, .. } => *profile,
        }
    }
    /// Exact final native program/forest identity.
    pub const fn program(&self) -> &[u8; 32] {
        match &self.observation {
            Observation::Catalog(catalog) => catalog.program(),
            Observation::NotRecordedCapacity { program, .. } => program,
        }
    }
    /// Complete captured source count.
    pub const fn source_count(&self) -> u32 {
        match &self.observation {
            Observation::Catalog(catalog) => catalog.source_count(),
            Observation::NotRecordedCapacity { source_count, .. } => *source_count,
        }
    }
    /// Exact semantic artifact count, unaffected by explanation capacity.
    pub const fn completed(&self) -> u32 {
        match &self.observation {
            Observation::Catalog(catalog) => catalog.completed(),
            Observation::NotRecordedCapacity { completed, .. } => *completed,
        }
    }
    /// Stable recording identity, distinct from the outer CAS envelope identity.
    pub fn identity(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.compiler-coverage-observation.v1\0");
        match &self.observation {
            Observation::Catalog(catalog) => {
                hash.update(&[0]);
                hash.update(&catalog.identity());
            }
            Observation::NotRecordedCapacity { .. } => {
                hash.update(&self.encode());
            }
        }
        *hash.finalize().as_bytes()
    }
    /// Owned immutable data charged by bounded residence.
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self
                .catalog()
                .map_or(0, SourceCoverageManifest::retained_bytes)
    }
    /// Exact canonical all-or-unavailable payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(OBSERVATION_MAGIC);
        match &self.observation {
            Observation::Catalog(catalog) => {
                out.push(0);
                out.extend_from_slice(&catalog.encode());
            }
            Observation::NotRecordedCapacity {
                input,
                profile,
                program,
                source_count,
                completed,
                reason,
            } => {
                out.push(1);
                out.extend_from_slice(input);
                out.extend_from_slice(profile);
                out.extend_from_slice(program);
                out.extend_from_slice(&source_count.to_le_bytes());
                out.extend_from_slice(&completed.to_le_bytes());
                out.push(reason.tag());
            }
        }
        out
    }
    /// Admits capacity explicitly; missing/trailing cause rows never become a catalog.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_SOURCE_COVERAGE_BYTES {
            return Err("coverage recording capacity exceeded");
        }
        let mut d = Decoder(bytes);
        if d.take(8)? != OBSERVATION_MAGIC {
            return Err("invalid coverage observation magic");
        }
        match d.take(1)?[0] {
            0 => Self::from_catalog(SourceCoverageManifest::decode(d.0)?),
            1 => {
                let input = d.array()?;
                let profile = d.array()?;
                let program = d.array()?;
                let count = d.u32()?;
                let completed = d.u32()?;
                let reason = SourceCoverageCapacityReason::from_tag(d.take(1)?[0])?;
                if !d.0.is_empty() {
                    return Err("trailing capacity observation bytes");
                }
                Self::not_recorded_capacity(input, profile, program, count, completed, reason)
            }
            _ => Err("invalid coverage observation tag"),
        }
    }
}

struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let v = self.0.get(..n).ok_or("short coverage encoding")?;
        self.0 = &self.0[n..];
        Ok(v)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        self.take(N)?
            .try_into()
            .map_err(|_| "short coverage identity")
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.array()?))
    }
}

/// Exact compiler-observed reason one captured source has no semantic artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCoverageGapCause {
    /// Rust-analyzer found the source in the package VFS but outside every active Cargo target.
    RustSourceOutsideActiveCargoTarget,
    /// Exact captured source is outside the selected TypeScript Compiler API program.
    TypeScriptSourceExcludedFromConfiguredProgram,
    /// Checking or lowering this source spent its work allowance (or the program's
    /// remaining allowance), so it has no semantic artifact. Every other source of
    /// the package keeps the facts it would have had.
    TypeScriptSourceWorkBudgetExhausted,
    /// Exact Python source was captured but its grammar rejected semantic lowering.
    PythonSourceSyntaxUnavailable,
    /// Exact Python source was captured but its declared codec was unavailable.
    PythonSourceEncodingUnavailable,
    /// Native solved dependencies reach unavailable captured Python resources.
    PythonSourceDependencyUnavailable,
    /// The language authority (loader, checker or analyzer) refused this exact source; every
    /// other source of the package keeps the facts it would have had.
    SourceAuthorityRefused,
    /// The native compiler rejected this exact source's syntax or flooded its diagnostics.
    SourceRejectedByNativeCompiler,
    /// Lowering or compact-IR construction cannot represent this exact source.
    SourceNotRepresentable,
    /// The package's compile deadline passed before this source finished or could start.
    CompileDeadlineExceeded,
}

impl std::fmt::Display for SourceCoverageGapCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RustSourceOutsideActiveCargoTarget => "outside every active Cargo target",
            Self::TypeScriptSourceExcludedFromConfiguredProgram => {
                "excluded from the configured TypeScript program"
            }
            Self::TypeScriptSourceWorkBudgetExhausted => {
                "its TypeScript file work allowance was exhausted"
            }
            Self::PythonSourceSyntaxUnavailable => "its Python syntax was rejected",
            Self::PythonSourceEncodingUnavailable => "its Python source encoding is unavailable",
            Self::PythonSourceDependencyUnavailable => {
                "it depends on Python resources that were not captured"
            }
            Self::SourceAuthorityRefused => "its language authority refused it",
            Self::SourceRejectedByNativeCompiler => "the native compiler rejected it",
            Self::SourceNotRepresentable => "lowering cannot represent it",
            Self::CompileDeadlineExceeded => "the compile deadline passed before it finished",
        })
    }
}

impl SourceCoverageGapCause {
    /// Rejects causes that cannot be produced by a TypeScript-language attempt.
    pub const fn is_typescript(self) -> bool {
        !matches!(
            self,
            Self::RustSourceOutsideActiveCargoTarget
                | Self::PythonSourceSyntaxUnavailable
                | Self::PythonSourceEncodingUnavailable
                | Self::PythonSourceDependencyUnavailable
        )
    }
    fn tag(self) -> u8 {
        match self {
            Self::RustSourceOutsideActiveCargoTarget => 1,
            Self::TypeScriptSourceExcludedFromConfiguredProgram => 2,
            Self::TypeScriptSourceWorkBudgetExhausted => 3,
            Self::PythonSourceSyntaxUnavailable => 4,
            Self::PythonSourceEncodingUnavailable => 5,
            Self::PythonSourceDependencyUnavailable => 6,
            Self::SourceAuthorityRefused => 7,
            Self::SourceRejectedByNativeCompiler => 8,
            Self::SourceNotRepresentable => 9,
            Self::CompileDeadlineExceeded => 10,
        }
    }
    fn from_tag(tag: u8) -> Result<Self, &'static str> {
        Ok(match tag {
            1 => Self::RustSourceOutsideActiveCargoTarget,
            2 => Self::TypeScriptSourceExcludedFromConfiguredProgram,
            3 => Self::TypeScriptSourceWorkBudgetExhausted,
            4 => Self::PythonSourceSyntaxUnavailable,
            5 => Self::PythonSourceEncodingUnavailable,
            6 => Self::PythonSourceDependencyUnavailable,
            7 => Self::SourceAuthorityRefused,
            8 => Self::SourceRejectedByNativeCompiler,
            9 => Self::SourceNotRepresentable,
            10 => Self::CompileDeadlineExceeded,
            _ => return Err("invalid coverage cause"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(bytes: &[u8]) -> [u8; 32] {
        *ContentId::<SourceFactDomain>::from_canonical_bytes(bytes).as_ref()
    }
    fn rows() -> Vec<SourceCoverageRow> {
        vec![
            SourceCoverageRow::Configuration {
                path: "tsconfig.app.json".into(),
                source: source(b"app config"),
                environment: [7; 32],
            },
            SourceCoverageRow::Gap {
                path: "src/app.spec.ts".into(),
                source: source(b"spec"),
                byte_len: 4,
                cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                diagnostic: None,
            },
            SourceCoverageRow::Gap {
                path: "src/slow.ts".into(),
                source: source(b"slow"),
                byte_len: 4,
                cause: SourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted,
                diagnostic: None,
            },
        ]
    }

    #[test]
    fn exact_tsz_cycle_owner_context_survives_coverage_wire_and_json() {
        let source_text = "export default function Layout() {}\n";
        let name_start = source_text.find("Layout").expect("fixture owner name") as u32;
        let name_end = name_start + "Layout".len() as u32;
        let row = SourceCoverageRow::Gap {
            path: "src/app/layout.tsx".into(),
            source: source(source_text.as_bytes()),
            byte_len: source_text.len() as u32,
            cause: SourceCoverageGapCause::SourceNotRepresentable,
            diagnostic: Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
                type_id: 0xfeed,
                owner_start: 0,
                owner_end: source_text.len() as u32 - 1,
                owner_name_start: name_start,
                owner_name_end: name_end,
                line: 1,
                column: 1,
                owner_name: "Layout".into(),
            }),
        };
        let catalog = SourceCoverageManifest::new([3; 32], [1, 1], [7; 32], 1, 0, vec![row])
            .expect("exact source-bound diagnostic is admitted");
        let wire = catalog.encode();
        let decoded = SourceCoverageManifest::decode(&wire).expect("diagnostic wire round-trips");
        assert_eq!(decoded, catalog);
        let json = serde_json::to_string(&catalog.rows()[0]).expect("coverage JSON");
        let projected: SourceCoverageRow =
            serde_json::from_str(&json).expect("coverage diagnostic JSON round-trips");
        assert_eq!(projected, catalog.rows()[0]);
        let Some(SourceCoverageRow::Gap {
            path,
            diagnostic: Some(diagnostic),
            ..
        }) = decoded.rows().first()
        else {
            panic!("diagnostic remains attached to exact source gap")
        };
        assert_eq!(path, "src/app/layout.tsx");
        assert!(diagnostic.summary().contains("owner declaration \"Layout\" at 1:1"));
        assert!(diagnostic.summary().contains("bytes 0..35"));
        assert!(!diagnostic.summary().contains("recursive edge"));

        let mut truncated = wire.clone();
        truncated.pop();
        assert!(SourceCoverageManifest::decode(&truncated).is_err());

        let mut invalid_name_end = wire;
        let name_end_offset = 86 + 1 + 4 + "src/app/layout.tsx".len() + 32 + 4 + 1 + 4 + 4 + 4 + 4;
        invalid_name_end[name_end_offset..name_end_offset + 4]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(SourceCoverageManifest::decode(&invalid_name_end).is_err());
    }
    #[test]
    fn coverage_capacity_forecast_is_borrowed_complete_and_not_a_truncated_catalog() {
        use SourceCoverageCapacityReason as Reason;
        let config = ["tsconfig.app.json"];
        assert_eq!(
            SourceCoverageObservation::forecast(config, ["src/main.ts"]),
            Ok(None)
        );
        assert_eq!(
            SourceCoverageObservation::forecast(
                config,
                std::iter::repeat_n("src/outside.ts", 20_000)
            ),
            Ok(Some(Reason::RowCount))
        );
        let long = format!("{}.ts", "x".repeat(4096));
        assert_eq!(
            SourceCoverageObservation::forecast(config, [long.as_str()]),
            Ok(Some(Reason::PathBytes))
        );
        let path = format!("{}.ts", "x".repeat(4000));
        assert_eq!(
            SourceCoverageObservation::forecast(config, std::iter::repeat_n(path.as_str(), 2200)),
            Ok(Some(Reason::EncodedBytes))
        );
        // Structural failure has priority even after the row budget was exceeded.
        assert!(
            SourceCoverageObservation::forecast(
                config,
                std::iter::repeat_n("src/outside.ts", 20_000).chain(["../foreign.ts"])
            )
            .is_err()
        );
        for reason in [Reason::RowCount, Reason::PathBytes, Reason::EncodedBytes] {
            let observation = SourceCoverageObservation::not_recorded_capacity(
                [3; 32],
                [1, 0],
                [7; 32],
                20_001,
                1,
                reason,
            )
            .expect("capacity observation fixture is valid");
            let bytes = observation.encode();
            assert_eq!(bytes.len(), 84);
            let cold = SourceCoverageObservation::decode(&bytes)
                .expect("encoded capacity observation decodes");
            assert_eq!(cold, observation);
            assert_eq!(cold.source_count(), 20_001);
            assert_eq!(cold.completed(), 1);
            assert_eq!(cold.capacity_reason(), Some(reason));
            assert!(
                cold.catalog().is_none(),
                "no invented rows or next selector"
            );
            let other = SourceCoverageObservation::not_recorded_capacity(
                [4; 32],
                [1, 0],
                [7; 32],
                20_001,
                1,
                reason,
            )
            .expect("second capacity observation fixture is valid");
            assert_ne!(cold.identity(), other.identity());
            let mut trailing = bytes;
            trailing.push(0);
            assert!(SourceCoverageObservation::decode(&trailing).is_err());
        }
        let catalog = SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, rows())
            .expect("catalog fixture is valid");
        let recording = SourceCoverageObservation::from_catalog(catalog.clone())
            .expect("catalog observation fixture is valid");
        let cold = SourceCoverageObservation::decode(&recording.encode())
            .expect("encoded catalog observation decodes");
        assert_eq!(cold.catalog(), Some(&catalog));
        assert_eq!(cold.capacity_reason(), None);
    }

    #[test]
    fn coverage_codec_retains_config_source_identity_and_distinct_observed_causes() {
        let value = SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, rows())
            .expect("coverage manifest fixture is valid");
        let tsx = SourceCoverageManifest::new([3; 32], [1, 1], [7; 32], 50, 48, rows())
            .expect("TSX coverage manifest fixture is valid");
        assert_eq!(tsx.profile(), [1, 1]);
        assert_ne!(tsx.identity(), value.identity());
        let bytes = value.encode();
        let config_row_len = 1 + 4 + "tsconfig.app.json".len() + 32 + 32;
        assert_eq!(bytes[86 + config_row_len], 1, "legacy gap rows keep tag 1");
        let cold =
            SourceCoverageManifest::decode(&bytes).expect("encoded coverage manifest decodes");
        assert_eq!(cold, value);
        assert_eq!(cold.identity(), value.identity());
        for root in [[4; 32], [5; 32]] {
            assert_ne!(
                SourceCoverageManifest::new(root, [1, 0], [7; 32], 50, 48, rows())
                    .expect("root variant preserves the valid fixture shape")
                    .identity(),
                value.identity()
            );
        }
        let mut changed = rows();
        if let SourceCoverageRow::Gap { cause, .. } = &mut changed[1] {
            *cause = SourceCoverageGapCause::CompileDeadlineExceeded;
        }
        assert_ne!(
            SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, changed)
                .expect("changed gap cause preserves the valid fixture shape")
                .identity(),
            value.identity()
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(SourceCoverageManifest::decode(&trailing).is_err());
        for offset in [0, 40, 74, 82, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[offset] = 255;
            assert!(
                SourceCoverageManifest::decode(&bad).is_err(),
                "corrupt offset {offset}"
            );
        }
    }
    #[test]
    fn coverage_codec_refuses_duplicate_reordered_missing_and_unbounded_observations() {
        let mut duplicate = rows();
        duplicate.push(duplicate[2].clone());
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 47, duplicate).is_err());
        let mut unordered = rows();
        unordered.swap(1, 2);
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, unordered).is_err());
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 49, rows()).is_err());
        let mut invalid = rows();
        if let SourceCoverageRow::Gap { path, .. } = &mut invalid[1] {
            *path = "../foreign.ts".into();
        }
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, invalid).is_err());
        assert!(SourceCoverageManifest::decode(&vec![0; MAX_SOURCE_COVERAGE_BYTES + 1]).is_err());
        // Aggregate count cannot authenticate a foreign language's catalog.
        assert!(SourceCoverageManifest::new([3; 32], [2, 4], [7; 32], 50, 48, rows()).is_err());
    }
    #[test]
    fn typescript_catalog_rejects_foreign_language_causes_at_construction_and_decode() {
        for cause in [
            SourceCoverageGapCause::RustSourceOutsideActiveCargoTarget,
            SourceCoverageGapCause::PythonSourceSyntaxUnavailable,
            SourceCoverageGapCause::PythonSourceEncodingUnavailable,
            SourceCoverageGapCause::PythonSourceDependencyUnavailable,
        ] {
            let mut foreign = rows();
            if let SourceCoverageRow::Gap { cause: seen, .. } = &mut foreign[2] {
                *seen = cause;
            }
            assert!(
                SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, foreign).is_err()
            );
            let mut bytes = SourceCoverageManifest::new([3; 32], [1, 0], [7; 32], 50, 48, rows())
                .expect("valid manifest fixture encodes before corruption")
                .encode();
            *bytes
                .last_mut()
                .expect("encoded manifest has its final cause tag") = cause.tag();
            assert!(SourceCoverageManifest::decode(&bytes).is_err());
        }
    }
}
