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

/// Validated source membership with paths borrowed as UTF-8 bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedNativeProgramSourceRef<'a> {
    /// Validated UTF-8 native Program path bytes.
    pub program_path: &'a [u8],
    /// Exact captured source identity and extent.
    pub source: SourceIdentity,
    /// Validated UTF-8 package path bytes, when owned by the package.
    pub package_path: Option<&'a [u8]>,
}

/// Validate a canonical path without any uninterruptible input-sized scan.
pub(super) fn validate_borrowed_path<E>(
    bytes: &[u8],
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<(), BorrowedNativeProgramError<E>> {
    use BorrowedNativeProgramError::{Manifest, Stopped};
    let invalid = || Manifest(NativeProgramSourceManifestError::Encoding);
    if bytes.is_empty() || bytes[0] == b'/' {
        return Err(invalid());
    }
    let mut offset = 0;
    let mut part_len = 0usize;
    let mut only_dots = true;
    while offset < bytes.len() {
        checkpoint(1).map_err(Stopped)?;
        let mut end = (offset + 4096).min(bytes.len());
        if end < bytes.len() {
            let mut back = 0;
            while end > offset && bytes[end] & 0xc0 == 0x80 && back < 4 {
                end -= 1;
                back += 1;
            }
            if end == offset || back == 4 {
                return Err(invalid());
            }
        }
        let chunk = &bytes[offset..end];
        std::str::from_utf8(chunk).map_err(|_| invalid())?;
        for &byte in chunk {
            if byte == b'\\' || byte == 0 {
                return Err(invalid());
            }
            if byte == b'/' {
                if part_len == 0 || (only_dots && part_len <= 2) {
                    return Err(invalid());
                }
                part_len = 0;
                only_dots = true;
            } else {
                part_len += 1;
                only_dots &= byte == b'.';
            }
        }
        offset = end;
    }
    if part_len == 0 || (only_dots && part_len <= 2) {
        return Err(invalid());
    }
    Ok(())
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

/// Failure of a borrowed admission, separating malformed data from a stopped operation.
#[derive(Debug)]
pub enum BorrowedNativeProgramError<E> {
    /// The encoded manifest violates the existing admission contract.
    Manifest(NativeProgramSourceManifestError),
    /// The caller's shared work ledger, deadline, or cancellation stopped admission.
    Stopped(E),
    /// The caller did not provide the scratch requested by `scratch_bytes`.
    Scratch,
}

/// A validated single-program manifest borrowing its pinned canonical bytes.
#[derive(Clone, Copy)]
pub struct BorrowedNativeProgramSourceManifest<'a> {
    bytes: &'a [u8],
    program: [u8; 32],
    toolchain: ContentId<ToolchainDomain>,
    count: usize,
    packages: usize,
}

impl<'a> BorrowedNativeProgramSourceManifest<'a> {
    /// Exact maximum scratch bytes needed to admit this single envelope.
    pub fn scratch_bytes(bytes: &[u8]) -> Result<usize, NativeProgramSourceManifestError> {
        if bytes.len() < HEADER || bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut input = Input(bytes);
        if input.take(8)? != MAGIC || input.u16()? != VERSION {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        input.take(64)?;
        let count = input.u32()? as usize;
        if count == 0 || count > MAX_NATIVE_PROGRAM_SOURCES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        Ok(count * 8)
    }

    /// Validates without allocating. The caller supplies the pinned wire and
    /// accounted scratch; `checkpoint` debits the live operation and checks
    /// its actual cancellation and deadline. One work unit admits a bounded
    /// record step or a chunk of at most 4096 bytes of path/hash work.
    pub fn decode<E>(
        bytes: &'a [u8],
        scratch: &mut [u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, BorrowedNativeProgramError<E>> {
        use BorrowedNativeProgramError::{Manifest, Scratch, Stopped};
        let required = Self::scratch_bytes(bytes).map_err(Manifest)?;
        let count = required / 8;
        if scratch.len() < required {
            return Err(Scratch);
        }
        // Charge wire traversal before path validation, then check at each row.
        for _ in bytes.chunks(4096) {
            checkpoint(1).map_err(Stopped)?;
        }
        let mut input = Input(bytes);
        input.take(10).map_err(Manifest)?;
        let toolchain = ContentId::try_from(input.take(32).map_err(Manifest)?)
            .map_err(|_| Manifest(NativeProgramSourceManifestError::Encoding))?;
        let program: [u8; 32] = input
            .take(32)
            .map_err(Manifest)?
            .try_into()
            .map_err(|_| Manifest(NativeProgramSourceManifestError::Encoding))?;
        input.u32().map_err(Manifest)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"typescript-tsz-program-source-manifest-v1\0");
        let mut prior: Option<&[u8]> = None;
        let mut packages = 0;
        for _ in 0..count {
            checkpoint(1).map_err(Stopped)?;
            let row_offset = bytes.len() - input.0.len();
            let row = checked_borrowed_row(&mut input, &mut checkpoint)?;
            if let Some(path) = prior {
                if !less(path, row.program_path, &mut checkpoint).map_err(Stopped)? {
                    return Err(Manifest(NativeProgramSourceManifestError::Encoding));
                }
            }
            prior = Some(row.program_path);
            if let Some(path) = row.package_path {
                let offset = path.as_ptr() as usize - bytes.as_ptr() as usize;
                let entry = &mut scratch[packages * 8..packages * 8 + 8];
                entry[..4].copy_from_slice(&(offset as u32).to_le_bytes());
                entry[4..].copy_from_slice(&(row_offset as u32).to_le_bytes());
                packages += 1;
            }
            hash.update(&(row.program_path.len() as u32).to_le_bytes());
            for chunk in row.program_path.chunks(4096) {
                checkpoint(1).map_err(Stopped)?;
                hash.update(chunk);
            }
            hash.update(row.source.identity.as_ref());
        }
        if !input.0.is_empty() {
            return Err(Manifest(NativeProgramSourceManifestError::Encoding));
        }
        if hash.finalize().as_bytes() != &program {
            return Err(Manifest(NativeProgramSourceManifestError::ProgramIdentity));
        }
        // In-place heapsort avoids an allocation and meters each comparison.
        fn less<E>(
            a: &[u8],
            b: &[u8],
            checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
        ) -> Result<bool, E> {
            for (a, b) in a.chunks(4096).zip(b.chunks(4096)) {
                checkpoint(1)?;
                match a.cmp(b) {
                    std::cmp::Ordering::Less => return Ok(true),
                    std::cmp::Ordering::Greater => return Ok(false),
                    std::cmp::Ordering::Equal => {}
                }
            }
            checkpoint(1)?;
            Ok(a.len() < b.len())
        }
        fn key<'a>(bytes: &'a [u8], values: &[u8], index: usize) -> &'a [u8] {
            let entry = &values[index * 8..index * 8 + 8];
            let offset = u32::from_le_bytes(entry[..4].try_into().unwrap()) as usize;
            let length = u32::from_le_bytes(bytes[offset - 4..offset].try_into().unwrap()) as usize;
            &bytes[offset..offset + length]
        }
        fn swap(values: &mut [u8], a: usize, b: usize) {
            for i in 0..8 {
                values.swap(a * 8 + i, b * 8 + i);
            }
        }
        fn sift<E>(
            bytes: &[u8],
            values: &mut [u8],
            mut root: usize,
            end: usize,
            checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
        ) -> Result<(), E> {
            loop {
                let mut child = root * 2 + 1;
                if child >= end {
                    return Ok(());
                }
                if child + 1 < end
                    && less(
                        key(bytes, values, child),
                        key(bytes, values, child + 1),
                        checkpoint,
                    )?
                {
                    child += 1;
                }
                if !less(
                    key(bytes, values, root),
                    key(bytes, values, child),
                    checkpoint,
                )? {
                    return Ok(());
                }
                swap(values, root, child);
                root = child;
            }
        }
        let values = &mut scratch[..packages * 8];
        for root in (0..packages / 2).rev() {
            sift(bytes, values, root, packages, &mut checkpoint).map_err(Stopped)?;
        }
        for end in (1..packages).rev() {
            swap(values, 0, end);
            sift(bytes, values, 0, end, &mut checkpoint).map_err(Stopped)?;
        }
        for index in 1..packages {
            if !less(
                key(bytes, values, index - 1),
                key(bytes, values, index),
                &mut checkpoint,
            )
            .map_err(Stopped)?
            {
                return Err(Manifest(NativeProgramSourceManifestError::Encoding));
            }
        }
        Ok(Self {
            bytes,
            program,
            toolchain,
            count,
            packages,
        })
    }

    /// Exact native membership identity.
    pub const fn program(&self) -> [u8; 32] {
        self.program
    }
    /// Compiler toolchain bound by the manifest.
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    /// Visit validated rows while charging and checking the caller's operation.
    pub fn visit_sources<E>(
        &self,
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
        mut visit: impl FnMut(BorrowedNativeProgramSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut input = Input(&self.bytes[HEADER..]);
        for _ in 0..self.count {
            checkpoint(1)?;
            let row = raw_borrowed_row(&mut input).expect("validated manifest");
            visit(row)?;
        }
        Ok(())
    }
}

/// A validated single Program owning the exact caller-admitted wire allocation.
pub struct RetainedNativeProgramSourceManifest {
    bytes: Box<[u8]>,
    program: [u8; 32],
    toolchain: ContentId<ToolchainDomain>,
    count: usize,
}

impl RetainedNativeProgramSourceManifest {
    /// Consume an exact wire allocation and validate it before returning an owner.
    /// The caller must debit its shared storage ledger before allocating `bytes`
    /// and `scratch`, and keep the wire debit for this owner's lifetime.
    pub fn admit<E>(
        bytes: Box<[u8]>,
        scratch: &mut [u8],
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, BorrowedNativeProgramError<E>> {
        let view = BorrowedNativeProgramSourceManifest::decode(&bytes, scratch, checkpoint)?;
        let (program, toolchain, count) = (view.program, view.toolchain, view.count);
        Ok(Self {
            bytes,
            program,
            toolchain,
            count,
        })
    }
    /// Exact native membership identity.
    pub const fn program(&self) -> [u8; 32] {
        self.program
    }
    /// Compiler toolchain bound by the manifest.
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    /// Visit validated rows with the live operation's shared work checkpoint.
    pub fn visit_sources<'a, E>(
        &'a self,
        checkpoint: impl FnMut(u64) -> Result<(), E>,
        visit: impl FnMut(BorrowedNativeProgramSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        BorrowedNativeProgramSourceManifest {
            bytes: &self.bytes,
            program: self.program,
            toolchain: self.toolchain,
            count: self.count,
            packages: 0,
        }
        .visit_sources(checkpoint, visit)
    }
}

/// A single Program retaining its validated package-path index with its wire.
pub struct RetainedIndexedNativeProgramSourceManifest {
    bytes: Box<[u8]>,
    index: Box<[u8]>,
    program: [u8; 32],
    toolchain: ContentId<ToolchainDomain>,
    count: usize,
    packages: usize,
}

impl RetainedIndexedNativeProgramSourceManifest {
    /// Admit exact caller-allocated wire and index together under the shared ledger.
    /// The caller must reserve both allocations before allocation and keep their
    /// debit live for the lifetime of the returned owner.
    pub fn admit<E>(
        bytes: Box<[u8]>,
        mut index: Box<[u8]>,
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, BorrowedNativeProgramError<E>> {
        let required = BorrowedNativeProgramSourceManifest::scratch_bytes(&bytes)
            .map_err(BorrowedNativeProgramError::Manifest)?;
        if index.len() != required {
            return Err(BorrowedNativeProgramError::Scratch);
        }
        let view = BorrowedNativeProgramSourceManifest::decode(&bytes, &mut index, checkpoint)?;
        let (program, toolchain, count, packages) =
            (view.program, view.toolchain, view.count, view.packages);
        Ok(Self {
            bytes,
            index,
            program,
            toolchain,
            count,
            packages,
        })
    }
    /// Exact native membership identity.
    pub const fn program(&self) -> [u8; 32] {
        self.program
    }
    /// Compiler toolchain bound by the manifest.
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    /// Visit actual membership rows with the live operation's work checkpoint.
    pub fn visit_sources<'a, E>(
        &'a self,
        checkpoint: impl FnMut(u64) -> Result<(), E>,
        visit: impl FnMut(BorrowedNativeProgramSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        BorrowedNativeProgramSourceManifest {
            bytes: &self.bytes,
            program: self.program,
            toolchain: self.toolchain,
            count: self.count,
            packages: self.packages,
        }
        .visit_sources(checkpoint, visit)
    }
    /// Find the unique package owner without scanning all Program members.
    pub fn source_for_package_bytes<E>(
        &self,
        path: &[u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Option<SourceIdentity>, E> {
        checkpoint(1)?;
        fn cmp<E>(
            a: &[u8],
            b: &[u8],
            checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
        ) -> Result<std::cmp::Ordering, E> {
            for (a, b) in a.chunks(4096).zip(b.chunks(4096)) {
                checkpoint(1)?;
                let order = a.cmp(b);
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            checkpoint(1)?;
            Ok(a.len().cmp(&b.len()))
        }
        let key = |index: usize| {
            let entry = &self.index[index * 8..index * 8 + 8];
            let offset = u32::from_le_bytes(entry[..4].try_into().unwrap()) as usize;
            let length =
                u32::from_le_bytes(self.bytes[offset - 4..offset].try_into().unwrap()) as usize;
            (
                &self.bytes[offset..offset + length],
                u32::from_le_bytes(entry[4..].try_into().unwrap()) as usize,
            )
        };
        let mut low = 0;
        let mut high = self.packages;
        while low < high {
            let mid = low + (high - low) / 2;
            if cmp(key(mid).0, path, &mut checkpoint)?.is_lt() {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low == self.packages {
            return Ok(None);
        }
        let (candidate, row_offset) = key(low);
        if !cmp(candidate, path, &mut checkpoint)?.is_eq() {
            return Ok(None);
        }
        checkpoint(1)?;
        Ok(Some(
            admitted_borrowed_row(&self.bytes[row_offset..]).source,
        ))
    }
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
pub(super) struct Input<'a>(&'a [u8]);
fn raw_borrowed_row<'a>(
    input: &mut Input<'a>,
) -> Result<BorrowedNativeProgramSourceRef<'a>, NativeProgramSourceManifestError> {
    fn path<'a>(input: &mut Input<'a>) -> Result<&'a [u8], NativeProgramSourceManifestError> {
        let length = input.u32()? as usize;
        input.take(length)
    }
    let program_path = path(input)?;
    let identity = ContentId::<SourceFactDomain>::try_from(input.take(32)?)
        .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
    let byte_len = input.u32()?;
    let package_path = match input.take(1)?[0] {
        0 => None,
        1 => Some(path(input)?),
        _ => return Err(NativeProgramSourceManifestError::Encoding),
    };
    Ok(BorrowedNativeProgramSourceRef {
        program_path,
        source: SourceIdentity { identity, byte_len },
        package_path,
    })
}
fn checked_borrowed_row<'a, E>(
    input: &mut Input<'a>,
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<BorrowedNativeProgramSourceRef<'a>, BorrowedNativeProgramError<E>> {
    let row = raw_borrowed_row(input).map_err(BorrowedNativeProgramError::Manifest)?;
    validate_borrowed_path(row.program_path, checkpoint)?;
    if let Some(path) = row.package_path {
        validate_borrowed_path(path, checkpoint)?;
    }
    Ok(row)
}
pub(super) fn admitted_borrowed_row(bytes: &[u8]) -> BorrowedNativeProgramSourceRef<'_> {
    raw_borrowed_row(&mut Input(bytes)).expect("validated row")
}
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
    #[test]
    fn retained_indexed_single_program_bounds_package_lookup() {
        let source = SourceIdentity::from_bytes(&0u32.to_le_bytes()).unwrap();
        let rows = (0u32..4096)
            .map(|i| NativeProgramSource {
                program_path: format!("project/file{i:04}.ts"),
                package_path: Some(format!("src/file{:04}.ts", 4095 - i)),
                source: SourceIdentity::from_bytes(&i.to_le_bytes()).unwrap(),
            })
            .collect();
        let owned = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            rows,
        )
        .unwrap();
        let required =
            BorrowedNativeProgramSourceManifest::scratch_bytes(owned.as_bytes()).unwrap();
        assert!(matches!(
            RetainedIndexedNativeProgramSourceManifest::admit(
                owned.as_bytes().into(),
                vec![0; required - 1].into_boxed_slice(),
                |_| Ok::<_, ()>(())
            ),
            Err(BorrowedNativeProgramError::Scratch)
        ));
        let indexed = RetainedIndexedNativeProgramSourceManifest::admit(
            owned.as_bytes().into(),
            vec![0; required].into_boxed_slice(),
            |_| Ok::<_, ()>(()),
        )
        .unwrap();
        let mut work = 0;
        assert_eq!(
            indexed
                .source_for_package_bytes(b"src/file4095.ts", |_| {
                    work += 1;
                    Ok::<_, ()>(())
                })
                .unwrap(),
            Some(source)
        );
        assert!(work < 64, "lookup must remain logarithmic: {work}");
        assert_eq!(
            indexed
                .source_for_package_bytes(b"src/missing.ts", |_| Ok::<_, ()>(()))
                .unwrap(),
            None
        );
        assert!(
            indexed
                .source_for_package_bytes(b"src/file4095.ts", |_| Err::<(), _>("stopped"))
                .is_err()
        );
        let dependency = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![NativeProgramSource {
                program_path: "compiler/lib.ts".into(),
                package_path: None,
                source,
            }],
        )
        .unwrap();
        let required =
            BorrowedNativeProgramSourceManifest::scratch_bytes(dependency.as_bytes()).unwrap();
        let dependency = RetainedIndexedNativeProgramSourceManifest::admit(
            dependency.as_bytes().into(),
            vec![0; required].into_boxed_slice(),
            |_| Ok::<_, ()>(()),
        )
        .unwrap();
        assert_eq!(
            dependency
                .source_for_package_bytes(b"missing", |_| Ok::<_, ()>(()))
                .unwrap(),
            None
        );
        assert!(
            dependency
                .source_for_package_bytes(b"missing", |_| Err::<(), _>("stopped"))
                .is_err()
        );
    }
    #[test]
    fn borrowed_path_validation_handles_utf8_chunk_boundaries_and_stop() {
        for prefix in 4093..=4097 {
            for character in ["é", "€", "🦀"] {
                let mut boundary = vec![b'a'; prefix];
                boundary.extend_from_slice(character.as_bytes());
                boundary.extend_from_slice(b"/name.ts");
                assert!(valid_path(std::str::from_utf8(&boundary).unwrap()));
                assert!(validate_borrowed_path(&boundary, &mut |_| Ok::<_, ()>(())).is_ok());
            }
        }
        for invalid in [
            b"x/\xc0\x80".as_slice(),
            b"x/\xe2\x82",
            b"x/\xf0\x80\x80\x80",
        ] {
            assert!(std::str::from_utf8(invalid).is_err());
            assert!(validate_borrowed_path(invalid, &mut |_| Ok::<_, ()>(())).is_err());
        }
        let mut path = vec![b'a'; 4095];
        path.extend_from_slice("é/name.ts".as_bytes());
        assert!(validate_borrowed_path(&path, &mut |_| Ok::<_, ()>(())).is_ok());
        assert!(valid_path(std::str::from_utf8(&path).unwrap()));
        let mut checks = 0;
        assert!(matches!(
            validate_borrowed_path(&path, &mut |_| {
                checks += 1;
                if checks == 2 { Err(()) } else { Ok(()) }
            }),
            Err(BorrowedNativeProgramError::Stopped(()))
        ));
        path[4096] = b'x';
        assert!(matches!(
            validate_borrowed_path(&path, &mut |_| Ok::<_, ()>(())),
            Err(BorrowedNativeProgramError::Manifest(
                NativeProgramSourceManifestError::Encoding
            ))
        ));
        for invalid in [b"/x".as_slice(), b"a/../b", b"a//b", b"a/.", b"a\\b"] {
            assert!(validate_borrowed_path(invalid, &mut |_| Ok::<_, ()>(())).is_err());
        }
    }
    #[test]
    fn borrowed_admission_matches_owned_and_checks_budget() {
        let owned = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            rows(),
        )
        .unwrap();
        let mut scratch =
            vec![0; BorrowedNativeProgramSourceManifest::scratch_bytes(owned.as_bytes()).unwrap()];
        let view =
            BorrowedNativeProgramSourceManifest::decode(owned.as_bytes(), &mut scratch, |_| {
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(view.program(), owned.program());
        assert_eq!(view.toolchain(), owned.toolchain());
        let mut visited = Vec::new();
        view.visit_sources(
            |_| Ok::<_, ()>(()),
            |row| {
                visited.push(row);
                Ok(())
            },
        )
        .unwrap();
        let expected = owned
            .sources()
            .map(|row| BorrowedNativeProgramSourceRef {
                program_path: row.program_path.as_bytes(),
                source: row.source,
                package_path: row.package_path.map(str::as_bytes),
            })
            .collect::<Vec<_>>();
        assert_eq!(visited, expected);
        let retained = RetainedNativeProgramSourceManifest::admit(
            owned.as_bytes().into(),
            &mut scratch,
            |_| Ok::<_, ()>(()),
        )
        .unwrap();
        assert_eq!(retained.program(), owned.program());
        let mut retained_rows = Vec::new();
        retained
            .visit_sources(
                |_| Ok::<_, ()>(()),
                |row| {
                    retained_rows.push(row);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(retained_rows, visited);
        assert!(matches!(
            BorrowedNativeProgramSourceManifest::decode(
                owned.as_bytes(),
                &mut [],
                |_| Ok::<_, ()>(())
            ),
            Err(BorrowedNativeProgramError::Scratch)
        ));
        assert!(matches!(
            BorrowedNativeProgramSourceManifest::decode(
                owned.as_bytes(),
                &mut scratch,
                |_| Err(())
            ),
            Err(BorrowedNativeProgramError::Stopped(()))
        ));
        let mut corrupt = owned.as_bytes().to_vec();
        corrupt[42] ^= 1;
        let mut corrupt_scratch = vec![0; scratch.len()];
        assert!(matches!(
            BorrowedNativeProgramSourceManifest::decode(&corrupt, &mut corrupt_scratch, |_| Ok::<
                _,
                (),
            >(
                ()
            )),
            Err(BorrowedNativeProgramError::Manifest(
                NativeProgramSourceManifestError::ProgramIdentity
            ))
        ));
    }

    #[test]
    fn borrowed_admission_rejects_duplicate_package_mapping() {
        let owned = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![
                NativeProgramSource {
                    program_path: "src/a.ts".into(),
                    package_path: Some("pkg/a.ts".into()),
                    source: SourceIdentity::from_bytes(b"a").unwrap(),
                },
                NativeProgramSource {
                    program_path: "src/b.ts".into(),
                    package_path: Some("pkg/b.ts".into()),
                    source: SourceIdentity::from_bytes(b"b").unwrap(),
                },
            ],
        )
        .unwrap();
        let mut bytes = owned.as_bytes().to_vec();
        let position = bytes
            .windows(8)
            .position(|part| part == b"pkg/b.ts")
            .unwrap();
        bytes[position..position + 8].copy_from_slice(b"pkg/a.ts");
        assert!(NativeProgramSourceManifest::decode(&bytes).is_err());
        let mut scratch =
            vec![0; BorrowedNativeProgramSourceManifest::scratch_bytes(&bytes).unwrap()];
        assert!(matches!(
            BorrowedNativeProgramSourceManifest::decode(&bytes, &mut scratch, |_| Ok::<_, ()>(())),
            Err(BorrowedNativeProgramError::Manifest(
                NativeProgramSourceManifestError::Encoding
            ))
        ));
    }
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
