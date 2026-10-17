//! Compiler-observed coverage facts. These explain omissions without granting completeness.
use backend_version::{ContentId, Schema, SourceFactDomain};
use serde::{Deserialize, Serialize};

/// Same finite source universe as the native TypeScript program bridge.
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
    },
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
        Ok(())
    }
}

/// Canonical immutable, source-bound observations from one compiler invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceCoverageManifest {
    identity: [u8; 32],
    input_root: [u8; 32],
    profile: [u8; 2],
    source_count: u32,
    completed: u32,
    rows: Vec<SourceCoverageRow>,
}
impl SourceCoverageManifest {
    /// Admits producer facts. Config rows precede gap rows, each in strict path order.
    pub fn new(
        input_root: [u8; 32],
        profile: [u8; 2],
        source_count: u32,
        completed: u32,
        rows: Vec<SourceCoverageRow>,
    ) -> Result<Self, &'static str> {
        if input_root == [0; 32]
            || profile != [1, 0]
            || source_count == 0
            || source_count > 16_384
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
            input_root,
            profile,
            source_count,
            completed,
            rows,
        };
        let length = value
            .rows
            .iter()
            .try_fold(54usize, |length, row| {
                length
                    .checked_add(row.path().len())?
                    .checked_add(match row {
                        SourceCoverageRow::Configuration { .. } => 69,
                        SourceCoverageRow::Gap { .. } => 42,
                    })
            })
            .ok_or("coverage catalog length overflow")?;
        if length > MAX_SOURCE_COVERAGE_BYTES {
            return Err("coverage catalog capacity exceeded");
        }
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
    /// Exact owned row/vector/path storage charged by immutable residence.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.rows.capacity() * std::mem::size_of::<SourceCoverageRow>()
            + self
                .rows
                .iter()
                .map(|row| match row {
                    SourceCoverageRow::Configuration { path, .. }
                    | SourceCoverageRow::Gap { path, .. } => path.capacity(),
                })
                .sum::<usize>()
    }
    /// Exact canonical typed-object payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.input_root);
        out.extend_from_slice(&self.profile);
        out.extend_from_slice(&self.source_count.to_le_bytes());
        out.extend_from_slice(&self.completed.to_le_bytes());
        out.extend_from_slice(&(self.rows.len() as u32).to_le_bytes());
        for row in &self.rows {
            out.push(row.key().0);
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
                    ..
                } => {
                    out.extend_from_slice(source);
                    out.extend_from_slice(&byte_len.to_le_bytes());
                    out.push(cause.tag());
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
                1 => SourceCoverageRow::Gap {
                    path,
                    source,
                    byte_len: d.u32()?,
                    cause: SourceCoverageGapCause::from_tag(d.take(1)?[0])?,
                },
                _ => return Err("invalid coverage row tag"),
            });
        }
        if !d.0.is_empty() {
            return Err("trailing coverage bytes");
        }
        Self::new(root, profile, count, completed, facts)
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
            },
            SourceCoverageRow::Gap {
                path: "src/slow.ts".into(),
                source: source(b"slow"),
                byte_len: 4,
                cause: SourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted,
            },
        ]
    }
    #[test]
    fn coverage_codec_retains_config_source_identity_and_distinct_observed_causes() {
        let value = SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, rows()).unwrap();
        let bytes = value.encode();
        let cold = SourceCoverageManifest::decode(&bytes).unwrap();
        assert_eq!(cold, value);
        assert_eq!(cold.identity(), value.identity());
        for root in [[4; 32], [5; 32]] {
            assert_ne!(
                SourceCoverageManifest::new(root, [1, 0], 50, 48, rows())
                    .unwrap()
                    .identity(),
                value.identity()
            );
        }
        let mut changed = rows();
        if let SourceCoverageRow::Gap { cause, .. } = &mut changed[1] {
            *cause = SourceCoverageGapCause::CompileDeadlineExceeded;
        }
        assert_ne!(
            SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, changed)
                .unwrap()
                .identity(),
            value.identity()
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(SourceCoverageManifest::decode(&trailing).is_err());
        for offset in [0, 40, 42, 50, bytes.len() - 1] {
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
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], 50, 47, duplicate).is_err());
        let mut unordered = rows();
        unordered.swap(1, 2);
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, unordered).is_err());
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], 50, 49, rows()).is_err());
        let mut invalid = rows();
        if let SourceCoverageRow::Gap { path, .. } = &mut invalid[1] {
            *path = "../foreign.ts".into();
        }
        assert!(SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, invalid).is_err());
        assert!(SourceCoverageManifest::decode(&vec![0; MAX_SOURCE_COVERAGE_BYTES + 1]).is_err());
        // Aggregate count cannot authenticate a foreign language's catalog.
        assert!(SourceCoverageManifest::new([3; 32], [2, 4], 50, 48, rows()).is_err());
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
            assert!(SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, foreign).is_err());
            let mut bytes = SourceCoverageManifest::new([3; 32], [1, 0], 50, 48, rows())
                .unwrap()
                .encode();
            *bytes.last_mut().unwrap() = cause.tag();
            assert!(SourceCoverageManifest::decode(&bytes).is_err());
        }
    }
}
