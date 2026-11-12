//! Borrowed graph admission. All variable scratch is supplied by the caller.
use super::*;
use crate::ir::native_program_sources::{
    BorrowedNativeProgramError as Error, BorrowedNativeProgramSourceManifest as Leaf,
};

/// Exact upper bounds for caller-owned graph validation scratch.
#[derive(Clone, Copy, Debug)]
pub struct BorrowedProgramGraphScratch {
    pub nodes: usize,
    pub rows: usize,
    pub leaf_paths: usize,
}

/// A config node borrowed from the pinned graph wire.
#[derive(Clone, Copy)]
pub struct BorrowedProgramGraphNode<'a> {
    references: &'a [u8],
}

/// A graph verified against the same identities and topology as the owning decoder.
pub struct BorrowedProgramGraph<'a, 's> {
    bytes: &'a [u8],
    toolchain: ContentId<ToolchainDomain>,
    rows: &'s [Option<NativeProgramOwnedSourceRef<'a>>],
    images: &'a [u8],
    image_count: usize,
    identity: [u8; 32],
}

fn cmp<E>(
    a: &str,
    b: &str,
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<std::cmp::Ordering, E> {
    for (a, b) in a.as_bytes().chunks(4096).zip(b.as_bytes().chunks(4096)) {
        checkpoint(1)?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal {
            return Ok(order);
        }
    }
    checkpoint(1)?;
    Ok(a.len().cmp(&b.len()))
}

fn sort_rows<'a, E>(
    rows: &mut [Option<NativeProgramOwnedSourceRef<'a>>],
    package: bool,
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<(), E> {
    fn key<'a>(row: NativeProgramOwnedSourceRef<'a>, package: bool) -> &'a str {
        if package {
            row.package_path.unwrap_or("")
        } else {
            row.program_path
        }
    }
    fn sift<E>(
        rows: &mut [Option<NativeProgramOwnedSourceRef<'_>>],
        mut root: usize,
        end: usize,
        package: bool,
        checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
    ) -> Result<(), E> {
        loop {
            let mut child = root * 2 + 1;
            if child >= end {
                return Ok(());
            }
            if child + 1 < end
                && cmp(
                    key(rows[child].unwrap(), package),
                    key(rows[child + 1].unwrap(), package),
                    checkpoint,
                )?
                .is_lt()
            {
                child += 1;
            }
            if !cmp(
                key(rows[root].unwrap(), package),
                key(rows[child].unwrap(), package),
                checkpoint,
            )?
            .is_lt()
            {
                return Ok(());
            }
            rows.swap(root, child);
            root = child;
        }
    }
    for root in (0..rows.len() / 2).rev() {
        sift(rows, root, rows.len(), package, checkpoint)?;
    }
    for end in (1..rows.len()).rev() {
        rows.swap(0, end);
        sift(rows, 0, end, package, checkpoint)?;
    }
    Ok(())
}

impl<'a, 's> BorrowedProgramGraph<'a, 's> {
    /// Scan structural lengths and derive exact upper bounds before allocating scratch.
    pub fn scratch<E>(
        bytes: &'a [u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<BorrowedProgramGraphScratch, Error<E>> {
        use Error::{Manifest, Stopped};
        let invalid = NativeProgramSourceManifestError::Encoding;
        if bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(Manifest(NativeProgramSourceManifestError::Limit));
        }
        let mut input = Input(bytes);
        let magic = input.take(8).map_err(Manifest)?;
        if magic != MAGIC && magic != PROJECT_SET_MAGIC {
            return Err(Manifest(invalid));
        }
        input.take(32).map_err(Manifest)?;
        input.u32().map_err(Manifest)?;
        let nodes = input.u32().map_err(Manifest)? as usize;
        if nodes == 0 || nodes > MAX_NATIVE_PROGRAM_NODES {
            return Err(Manifest(NativeProgramSourceManifestError::Limit));
        }
        let mut rows = 0usize;
        let mut leaf_paths = 0usize;
        for _ in 0..nodes {
            checkpoint(1).map_err(Stopped)?;
            input.cell().map_err(Manifest)?;
            input.take(64).map_err(Manifest)?;
            let refs = input.u32().map_err(Manifest)? as usize;
            if refs > nodes {
                return Err(Manifest(NativeProgramSourceManifestError::Limit));
            }
            input.take(refs * 4 + 32).map_err(Manifest)?;
            let leaf = input.cell().map_err(Manifest)?;
            if !leaf.is_empty() {
                let count = Leaf::scratch_slots(leaf).map_err(Manifest)?;
                rows = rows
                    .checked_add(count)
                    .ok_or(Manifest(NativeProgramSourceManifestError::Limit))?;
                if rows > MAX_NATIVE_PROGRAM_SOURCES {
                    return Err(Manifest(NativeProgramSourceManifestError::Limit));
                }
                leaf_paths = leaf_paths.max(count);
            }
        }
        Ok(BorrowedProgramGraphScratch {
            nodes,
            rows,
            leaf_paths,
        })
    }

    /// Validate graph topology, membership and sealed images in caller-owned scratch.
    pub fn decode<E>(
        bytes: &'a [u8],
        nodes: &mut [Option<BorrowedProgramGraphNode<'a>>],
        rows: &'s mut [Option<NativeProgramOwnedSourceRef<'a>>],
        leaf_paths: &mut [Option<&'a str>],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, Error<E>> {
        use Error::{Manifest, Scratch, Stopped};
        let invalid = NativeProgramSourceManifestError::Encoding;
        let shape = Self::scratch(bytes, &mut checkpoint)?;
        if nodes.len() < shape.nodes
            || rows.len() < shape.rows
            || leaf_paths.len() < shape.leaf_paths
        {
            return Err(Scratch);
        }
        for _ in bytes.chunks(4096) {
            checkpoint(1).map_err(Stopped)?;
        }
        let mut input = Input(bytes);
        let project_set = input.take(8).map_err(Manifest)? == PROJECT_SET_MAGIC;
        let toolchain = ContentId::try_from(input.take(32).map_err(Manifest)?)
            .map_err(|_| Manifest(invalid))?;
        let root = input.u32().map_err(Manifest)? as usize;
        let count = input.u32().map_err(Manifest)? as usize;
        if root >= count {
            return Err(Manifest(NativeProgramSourceManifestError::Limit));
        }
        let mut previous = None;
        let mut row_count = 0;
        for index in 0..count {
            checkpoint(1).map_err(Stopped)?;
            let path = std::str::from_utf8(input.cell().map_err(Manifest)?)
                .map_err(|_| Manifest(invalid))?;
            if !valid_path(path) || previous.is_some_and(|p| p >= path) {
                return Err(Manifest(invalid));
            }
            previous = Some(path);
            let config = input.take(32).map_err(Manifest)?;
            let environment = input.take(32).map_err(Manifest)?;
            let reference_count = input.u32().map_err(Manifest)? as usize;
            let references = input.take(reference_count * 4).map_err(Manifest)?;
            for (i, chunk) in references.chunks_exact(4).enumerate() {
                checkpoint(1).map_err(Stopped)?;
                let value = u32::from_le_bytes(chunk.try_into().unwrap()) as usize;
                if value >= count {
                    return Err(Manifest(invalid));
                }
                for earlier in references[..i * 4].chunks_exact(4) {
                    checkpoint(1).map_err(Stopped)?;
                    if earlier == chunk {
                        return Err(Manifest(invalid));
                    }
                }
            }
            let expected: [u8; 32] = input.take(32).map_err(Manifest)?.try_into().unwrap();
            let member = input.cell().map_err(Manifest)?;
            let program = if member.is_empty() {
                None
            } else {
                let leaf = Leaf::decode(member, leaf_paths, &mut checkpoint)?;
                if leaf.toolchain() != toolchain {
                    return Err(Manifest(invalid));
                }
                Some(leaf)
            };
            if program.is_none() && reference_count == 0 {
                return Err(Manifest(invalid));
            }
            let mut hash = blake3::Hasher::new();
            hash.update(b"typescript.program-node.v1\0");
            hash.update(toolchain.as_ref());
            hash.update(&(path.len() as u64).to_le_bytes());
            for chunk in path.as_bytes().chunks(4096) {
                checkpoint(1).map_err(Stopped)?;
                hash.update(chunk);
            }
            hash.update(config);
            hash.update(environment);
            hash.update(&(reference_count as u64).to_le_bytes());
            hash.update(references);
            match program {
                Some(leaf) => {
                    hash.update(&[1]);
                    hash.update(&leaf.program());
                }
                None => {
                    hash.update(&[0]);
                }
            }
            if hash.finalize().as_bytes() != &expected {
                return Err(Manifest(NativeProgramSourceManifestError::ProgramIdentity));
            }
            if let Some(leaf) = program {
                leaf.visit_sources(&mut checkpoint, |row| {
                    rows[row_count] = Some(NativeProgramOwnedSourceRef {
                        program: leaf.program(),
                        node: Some(expected),
                        row,
                    });
                    row_count += 1;
                    Ok(())
                })
                .map_err(Stopped)?;
            }
            nodes[index] = Some(BorrowedProgramGraphNode { references });
        }
        fn visit<E>(
            index: usize,
            nodes: &[Option<BorrowedProgramGraphNode<'_>>],
            state: &mut [u8; MAX_NATIVE_PROGRAM_NODES],
            checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
        ) -> Result<(), Error<E>> {
            checkpoint(1).map_err(Error::Stopped)?;
            if state[index] == 1 {
                return Err(Error::Manifest(NativeProgramSourceManifestError::Encoding));
            }
            if state[index] == 2 {
                return Ok(());
            }
            state[index] = 1;
            for reference in nodes[index].unwrap().references.chunks_exact(4) {
                visit(
                    u32::from_le_bytes(reference.try_into().unwrap()) as usize,
                    nodes,
                    state,
                    checkpoint,
                )?;
            }
            state[index] = 2;
            Ok(())
        }
        let mut state = [0; MAX_NATIVE_PROGRAM_NODES];
        visit(root, &nodes[..count], &mut state, &mut checkpoint)?;
        if project_set {
            for index in 0..count {
                visit(index, &nodes[..count], &mut state, &mut checkpoint)?;
            }
        }
        if state[..count].iter().any(|s| *s != 2) {
            return Err(Manifest(invalid));
        }
        let rows = &mut rows[..row_count];
        sort_rows(rows, false, &mut checkpoint).map_err(Stopped)?;
        let mut prior: Option<NativeProgramOwnedSourceRef<'_>> = None;
        let mut owner = None;
        for &row in rows.iter() {
            checkpoint(1).map_err(Stopped)?;
            let row = row.unwrap();
            if let Some(prev) = prior {
                if cmp(prev.program_path, row.program_path, &mut checkpoint)
                    .map_err(Stopped)?
                    .is_eq()
                {
                    if prev.source != row.source
                        || (!project_set && prev.package_path != row.package_path)
                        || (owner.is_some()
                            && row.package_path.is_some()
                            && owner != row.package_path)
                    {
                        return Err(Manifest(invalid));
                    }
                    if owner.is_none() {
                        owner = row.package_path;
                    }
                } else {
                    owner = row.package_path;
                }
            } else {
                owner = row.package_path;
            }
            prior = Some(row);
        }
        sort_rows(rows, true, &mut checkpoint).map_err(Stopped)?;
        for pair in rows.windows(2) {
            let (a, b) = (pair[0].unwrap(), pair[1].unwrap());
            if let (Some(a), Some(b)) = (a.package_path, b.package_path) {
                if cmp(a, b, &mut checkpoint).map_err(Stopped)?.is_eq() {
                    return Err(Manifest(invalid));
                }
            }
        }
        let image_count = input.u32().map_err(Manifest)? as usize;
        if image_count > MAX_NATIVE_PROGRAM_SOURCES {
            return Err(Manifest(NativeProgramSourceManifestError::Limit));
        }
        let images = input.0;
        let mut prior_image = None;
        for _ in 0..image_count {
            checkpoint(1).map_err(Stopped)?;
            let path = std::str::from_utf8(input.cell().map_err(Manifest)?)
                .map_err(|_| Manifest(invalid))?;
            if prior_image.is_some_and(|previous| previous >= path) {
                return Err(Manifest(invalid));
            }
            prior_image = Some(path);
            let recorded_node: [u8; 32] = input.take(32).map_err(Manifest)?.try_into().unwrap();
            let image: [u8; 32] = input.take(32).map_err(Manifest)?.try_into().unwrap();
            SemanticImageIdentity::try_from(image).map_err(|_| Manifest(invalid))?;
            // Rows are sorted by package path. Binary search with metered comparisons.
            let mut low = 0;
            let mut high = rows.len();
            while low < high {
                let mid = low + (high - low) / 2;
                let key = rows[mid].unwrap().package_path.unwrap_or("");
                if cmp(key, path, &mut checkpoint).map_err(Stopped)?.is_lt() {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            if low == rows.len()
                || rows[low].unwrap().package_path != Some(path)
                || rows[low].unwrap().node != Some(recorded_node)
            {
                return Err(Manifest(invalid));
            }
        }
        if !input.0.is_empty() {
            return Err(Manifest(invalid));
        }
        let mut hash = blake3::Hasher::new();
        for chunk in bytes.chunks(4096) {
            checkpoint(1).map_err(Stopped)?;
            hash.update(chunk);
        }
        Ok(Self {
            bytes,
            toolchain,
            rows,
            images,
            image_count,
            identity: *hash.finalize().as_bytes(),
        })
    }
    pub const fn membership_identity(&self) -> [u8; 32] {
        self.identity
    }
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn visit_sources<E>(
        &self,
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
        mut visit: impl FnMut(NativeProgramOwnedSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        for row in self.rows {
            checkpoint(1)?;
            visit(row.unwrap())?;
        }
        Ok(())
    }
    pub fn image_matches<E>(
        &self,
        path: &str,
        image: &[u8; 32],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut input = Input(self.images);
        for _ in 0..self.image_count {
            checkpoint(1)?;
            let recorded = std::str::from_utf8(input.cell().expect("validated image path"))
                .expect("validated image path");
            input.take(32).expect("validated image node");
            let digest = input.take(32).expect("validated image digest");
            match cmp(recorded, path, &mut checkpoint)? {
                std::cmp::Ordering::Equal => return Ok(digest == image),
                std::cmp::Ordering::Greater => return Ok(false),
                std::cmp::Ordering::Less => {}
            }
        }
        Ok(false)
    }
}
