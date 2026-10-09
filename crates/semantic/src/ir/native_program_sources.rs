//! Exact native TypeScript program membership, stored once per compiler closure.
//!
//! This is a source-membership witness, not a complete-read or complete-fact
//! certificate. Package mappings come from the admitted compiler API handoff;
//! SDK and dependency members retain their captured immutable identities.

use crate::ir::{SourceIdentity, typescript_program_identity};
use backend_version::{ContentId, Schema, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeSet;
use std::sync::Arc;
use thiserror::Error;

/// Same finite source count admitted by the selected compiler API bridge.
pub const MAX_NATIVE_PROGRAM_SOURCES: usize = 16_384;
/// Same finite payload budget admitted for that bridge's program report.
pub const MAX_NATIVE_PROGRAM_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"BKNPSM01";
const VERSION: u16 = 1;
const HEADER: usize = 8 + 2 + 32 + 32 + 4;

/// CAS schema for one canonical native program source manifest.
pub struct NativeProgramSourceManifestSchema;
impl Schema for NativeProgramSourceManifestSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc009;
    const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// One actual bound native program file and its exact captured package mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProgramSource {
    /// Path used by TSZ's bound program and native declaration coordinates.
    pub program_path: String,
    /// Identity of the exact text retained in that native bound source file.
    pub source: SourceIdentity,
    /// Exact package-relative source path, when this is a captured package source.
    pub package_path: Option<String>,
}

/// Borrowed membership row; no compiler handles or path guesses are persisted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeProgramSourceRef<'a> {
    /// Native bound-file path.
    pub program_path: &'a str,
    /// Exact native source identity and extent.
    pub source: SourceIdentity,
    /// Original admitted package-relative mapping, or an immutable dependency.
    pub package_path: Option<&'a str>,
}

/// Validated canonical bytes of one full native TypeScript program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProgramSourceManifest {
    bytes: Arc<[u8]>,
    program: [u8; 32],
    toolchain: ContentId<ToolchainDomain>,
    count: usize,
}

/// Closed source-membership admission failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NativeProgramSourceManifestError {
    /// Unsupported version, malformed cells, ordering, paths or trailing bytes.
    #[error("invalid native program source manifest encoding")]
    Encoding,
    /// Source count or encoded bytes exceed the compiler's admitted bounds.
    #[error("native program source manifest exceeds its admitted bounds")]
    Limit,
    /// The independently hashed native membership differs from the retained ID.
    #[error("native program source manifest identity mismatch")]
    ProgramIdentity,
}

impl NativeProgramSourceManifest {
    /// Constructs membership from actual native files. No caller supplies a program ID.
    pub fn from_sources(
        toolchain: ContentId<ToolchainDomain>,
        mut sources: Vec<NativeProgramSource>,
    ) -> Result<Self, NativeProgramSourceManifestError> {
        if sources.is_empty() || sources.len() > MAX_NATIVE_PROGRAM_SOURCES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let encoded_bytes = sources
            .iter()
            .try_fold(HEADER, |total, row| {
                total
                    .checked_add(4)?
                    .checked_add(row.program_path.len())?
                    .checked_add(37)?
                    .checked_add(row.package_path.as_ref().map_or(0, |path| 4 + path.len()))
            })
            .ok_or(NativeProgramSourceManifestError::Limit)?;
        if encoded_bytes > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        sources.sort_unstable_by(|a, b| a.program_path.cmp(&b.program_path));
        let members = sources
            .iter()
            .map(|s| (s.program_path.clone(), *s.source.identity))
            .collect::<Vec<_>>();
        let program = typescript_program_identity(&members)
            .ok_or(NativeProgramSourceManifestError::Encoding)?;
        let mut bytes = Vec::with_capacity(encoded_bytes);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(toolchain.as_ref());
        bytes.extend_from_slice(&program);
        bytes.extend_from_slice(&(sources.len() as u32).to_le_bytes());
        for source in sources {
            put_text(&mut bytes, &source.program_path)?;
            bytes.extend_from_slice(source.source.identity.as_ref());
            bytes.extend_from_slice(&source.source.byte_len.to_le_bytes());
            match source.package_path {
                None => bytes.push(0),
                Some(path) => {
                    bytes.push(1);
                    put_text(&mut bytes, &path)?;
                }
            }
            if bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
                return Err(NativeProgramSourceManifestError::Limit);
            }
        }
        Self::decode(&bytes)
    }

    /// Validates the closed codec and recomputes membership before retaining bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeProgramSourceManifestError> {
        if bytes.len() < HEADER || bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut input = Input(bytes);
        if input.take(8)? != MAGIC || input.u16()? != VERSION {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        let toolchain = ContentId::<ToolchainDomain>::try_from(input.take(32)?)
            .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
        let program = input
            .take(32)?
            .try_into()
            .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
        let count = input.u32()? as usize;
        if count == 0 || count > MAX_NATIVE_PROGRAM_SOURCES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut members = Vec::with_capacity(count);
        let mut mappings = BTreeSet::new();
        let mut prior = None;
        for _ in 0..count {
            let row = input.row()?;
            if prior.is_some_and(|path| path >= row.program_path)
                || row.package_path.is_some_and(|path| !mappings.insert(path))
            {
                return Err(NativeProgramSourceManifestError::Encoding);
            }
            prior = Some(row.program_path);
            members.push((row.program_path.to_owned(), *row.source.identity));
        }
        if !input.0.is_empty() {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        if typescript_program_identity(&members) != Some(program) {
            return Err(NativeProgramSourceManifestError::ProgramIdentity);
        }
        Ok(Self {
            bytes: Arc::from(bytes),
            program,
            toolchain,
            count,
        })
    }

    /// Independently verified full native membership identity.
    #[must_use]
    pub const fn program(&self) -> [u8; 32] {
        self.program
    }
    /// Compiler toolchain bound by the admitted package invocation.
    #[must_use]
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    /// Canonical CAS payload; publication may stream its existing bounded chunks.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Iterates already validated membership without copying source paths.
    pub fn sources(&self) -> impl ExactSizeIterator<Item = NativeProgramSourceRef<'_>> {
        Sources {
            input: Input(&self.bytes[HEADER..]),
            remaining: self.count,
        }
    }
}

fn put_text(out: &mut Vec<u8>, value: &str) -> Result<(), NativeProgramSourceManifestError> {
    if !valid_path(value) {
        return Err(NativeProgramSourceManifestError::Encoding);
    }
    let len = u32::try_from(value.len()).map_err(|_| NativeProgramSourceManifestError::Limit)?;
    if out
        .len()
        .checked_add(value.len())
        .and_then(|n| n.checked_add(4))
        .is_none_or(|n| n > MAX_NATIVE_PROGRAM_MANIFEST_BYTES)
    {
        return Err(NativeProgramSourceManifestError::Limit);
    }
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|part| part == ".." || part.is_empty() || part == ".")
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], NativeProgramSourceManifestError> {
        let (head, tail) = self
            .0
            .split_at_checked(count)
            .ok_or(NativeProgramSourceManifestError::Encoding)?;
        self.0 = tail;
        Ok(head)
    }
    fn u16(&mut self) -> Result<u16, NativeProgramSourceManifestError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, NativeProgramSourceManifestError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?,
        ))
    }
    fn text(&mut self) -> Result<&'a str, NativeProgramSourceManifestError> {
        let len = self.u32()? as usize;
        let text = std::str::from_utf8(self.take(len)?)
            .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
        if !valid_path(text) {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        Ok(text)
    }
    fn row(&mut self) -> Result<NativeProgramSourceRef<'a>, NativeProgramSourceManifestError> {
        let program_path = self.text()?;
        let identity = ContentId::<SourceFactDomain>::try_from(self.take(32)?)
            .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
        let byte_len = self.u32()?;
        let package_path = match self.take(1)?[0] {
            0 => None,
            1 => Some(self.text()?),
            _ => return Err(NativeProgramSourceManifestError::Encoding),
        };
        Ok(NativeProgramSourceRef {
            program_path,
            source: SourceIdentity { identity, byte_len },
            package_path,
        })
    }
}
struct Sources<'a> {
    input: Input<'a>,
    remaining: usize,
}
impl<'a> Iterator for Sources<'a> {
    type Item = NativeProgramSourceRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some(
            self.input
                .row()
                .expect("retained native program manifest was validated"),
        )
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for Sources<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<NativeProgramSource> {
        [
            (
                "workspace/main.ts",
                b"export {};".as_slice(),
                Some("main.ts"),
            ),
            (
                "@compiler/lib.es5.d.ts",
                b"interface Object {}".as_slice(),
                None,
            ),
        ]
        .into_iter()
        .map(|(program_path, bytes, package_path)| NativeProgramSource {
            program_path: program_path.into(),
            source: SourceIdentity::from_bytes(bytes).expect("bounded fixture"),
            package_path: package_path.map(str::to_owned),
        })
        .collect()
    }
    #[test]
    fn full_program_manifest_retains_library_members_and_canonical_identity() {
        let toolchain = ContentId::from_canonical_bytes(b"selected compiler");
        let manifest =
            NativeProgramSourceManifest::from_sources(toolchain, rows()).expect("full membership");
        let mut reversed = rows();
        reversed.reverse();
        assert_eq!(
            NativeProgramSourceManifest::from_sources(toolchain, reversed)
                .expect("canonical order"),
            manifest
        );
        assert_eq!(manifest.sources().count(), 2);
        assert_eq!(
            manifest
                .sources()
                .filter(|row| row.package_path.is_some())
                .count(),
            1
        );
        assert_eq!(
            NativeProgramSourceManifest::decode(manifest.as_bytes()).expect("round trip"),
            manifest
        );
        let mut sdk_edit = rows();
        sdk_edit[1].source =
            SourceIdentity::from_bytes(b"interface Object { x: number }").expect("source");
        assert_ne!(
            NativeProgramSourceManifest::from_sources(toolchain, sdk_edit)
                .expect("edited SDK")
                .program(),
            manifest.program()
        );
    }
    #[test]
    fn full_program_manifest_closes_version_identity_mapping_and_extent() {
        let toolchain = ContentId::from_canonical_bytes(b"selected compiler");
        let manifest =
            NativeProgramSourceManifest::from_sources(toolchain, rows()).expect("manifest");
        let mut wrong_id = manifest.as_bytes().to_vec();
        wrong_id[42] ^= 1;
        assert_eq!(
            NativeProgramSourceManifest::decode(&wrong_id),
            Err(NativeProgramSourceManifestError::ProgramIdentity)
        );
        let mut version = manifest.as_bytes().to_vec();
        version[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            NativeProgramSourceManifest::decode(&version),
            Err(NativeProgramSourceManifestError::Encoding)
        );
        let mut trailing = manifest.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            NativeProgramSourceManifest::decode(&trailing),
            Err(NativeProgramSourceManifestError::Encoding)
        );
        let mut duplicated = rows();
        duplicated[1].package_path = Some("main.ts".into());
        assert_eq!(
            NativeProgramSourceManifest::from_sources(toolchain, duplicated),
            Err(NativeProgramSourceManifestError::Encoding)
        );
        let mut wrong_path = rows();
        wrong_path[0].program_path = "../main.ts".into();
        assert_eq!(
            NativeProgramSourceManifest::from_sources(toolchain, wrong_path),
            Err(NativeProgramSourceManifestError::Encoding)
        );
        assert_eq!(
            NativeProgramSourceManifest::decode(&manifest.as_bytes()[..40]),
            Err(NativeProgramSourceManifestError::Limit)
        );
        let mut count = manifest.as_bytes().to_vec();
        count[74..78].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            NativeProgramSourceManifest::decode(&count),
            Err(NativeProgramSourceManifestError::Limit)
        );
        let mut oversized = rows();
        oversized[0].program_path = "a".repeat(MAX_NATIVE_PROGRAM_MANIFEST_BYTES);
        assert_eq!(
            NativeProgramSourceManifest::from_sources(toolchain, oversized),
            Err(NativeProgramSourceManifestError::Limit)
        );
    }
}
