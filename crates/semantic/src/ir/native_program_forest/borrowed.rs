//! Borrowed graph admission. All variable scratch is supplied by the caller.
use super::*;
use crate::ir::native_program_sources::{
    BorrowedNativeProgramError as Error, BorrowedNativeProgramSourceManifest as Leaf,
    BorrowedNativeProgramSourceRef, admitted_borrowed_row, validate_borrowed_path,
};

/// One source row and its independently verified leaf/config identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedOwnedNativeProgramSourceRef<'a> {
    /// Leaf Program identity.
    pub program: [u8; 32],
    /// Config node identity for this leaf.
    pub node: Option<[u8; 32]>,
    /// Validated byte-path source membership.
    pub row: BorrowedNativeProgramSourceRef<'a>,
}
impl<'a> std::ops::Deref for BorrowedOwnedNativeProgramSourceRef<'a> {
    type Target = BorrowedNativeProgramSourceRef<'a>;
    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

/// Exact upper bounds for caller-owned graph validation scratch.
#[derive(Clone, Copy, Debug)]
pub struct BorrowedProgramGraphScratch {
    /// Number of graph nodes (each requires eight scratch bytes).
    pub nodes: usize,
    /// Number of source rows (each requires sixteen scratch bytes).
    pub rows: usize,
    /// Scratch bytes required for the largest individual leaf.
    pub leaf_paths: usize,
}

impl BorrowedProgramGraphScratch {
    /// Exact bytes needed for the node index.
    pub const fn node_bytes(self) -> usize {
        self.nodes * 8
    }
    /// Exact bytes needed for the source-row index.
    pub const fn row_bytes(self) -> usize {
        self.rows * 16
    }
}

/// A graph verified against the same identities and topology as the owning decoder.
pub struct BorrowedProgramGraph<'a, 's> {
    bytes: &'a [u8],
    toolchain: ContentId<ToolchainDomain>,
    rows: &'s [u8],
    identity: [u8; 32],
}

/// Validated graph wire and offset index retained under the caller's ledger.
pub struct RetainedProgramGraph {
    bytes: Box<[u8]>,
    rows: Box<[u8]>,
    toolchain: ContentId<ToolchainDomain>,
    identity: [u8; 32],
}

impl RetainedProgramGraph {
    /// Validate caller-owned exact allocations before transferring them to the owner.
    /// The caller must reserve the shared storage ledger before allocating and
    /// retain the wire and row-index debit for this owner's lifetime.
    pub fn admit<E>(
        bytes: Box<[u8]>,
        mut rows: Box<[u8]>,
        nodes: &mut [u8],
        leaf_paths: &mut [u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, Error<E>> {
        let shape = BorrowedProgramGraph::scratch(&bytes, &mut checkpoint)?;
        if rows.len() != shape.row_bytes() {
            return Err(Error::Scratch);
        }
        let view = BorrowedProgramGraph::decode(&bytes, nodes, &mut rows, leaf_paths, checkpoint)?;
        let (toolchain, identity) = (view.toolchain, view.identity);
        Ok(Self {
            bytes,
            rows,
            toolchain,
            identity,
        })
    }
    fn view(&self) -> BorrowedProgramGraph<'_, '_> {
        BorrowedProgramGraph {
            bytes: &self.bytes,
            toolchain: self.toolchain,
            rows: &self.rows,
            identity: self.identity,
        }
    }
    /// Stable identity of the entire canonical graph envelope.
    pub const fn membership_identity(&self) -> [u8; 32] {
        self.identity
    }
    /// Compiler toolchain shared by all leaf programs.
    pub const fn toolchain(&self) -> ContentId<ToolchainDomain> {
        self.toolchain
    }
    /// Visit actual leaf memberships with the live shared work checkpoint.
    pub fn visit_sources<'a, E>(
        &'a self,
        checkpoint: impl FnMut(u64) -> Result<(), E>,
        visit: impl FnMut(BorrowedOwnedNativeProgramSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.view().visit_sources(checkpoint, visit)
    }
    /// Verify one producer-sealed image digest with metered lookup.
    pub fn image_matches<E>(
        &self,
        path: &str,
        image: &[u8; 32],
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        self.image_matches_bytes(path.as_bytes(), image, checkpoint)
    }
    /// Verify a producer-sealed image against borrowed path bytes with metered lookup.
    pub fn image_matches_bytes<E>(
        &self,
        path: &[u8],
        image: &[u8; 32],
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        self.view().image_matches_bytes(path, image, checkpoint)
    }
    /// Find the unique package source using the validated retained row index.
    pub fn source_for_package_bytes<E>(
        &self,
        path: &[u8],
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Option<super::super::SourceIdentity>, E> {
        self.view().source_for_package_bytes(path, checkpoint)
    }
}

fn cmp<E>(
    a: &[u8],
    b: &[u8],
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<std::cmp::Ordering, E> {
    for (a, b) in a.chunks(4096).zip(b.chunks(4096)) {
        checkpoint(1)?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal {
            return Ok(order);
        }
    }
    checkpoint(1)?;
    Ok(a.len().cmp(&b.len()))
}

fn get_u32(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}
fn put_u32(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
}
fn row_at<'a>(
    wire: &'a [u8],
    rows: &[u8],
    index: usize,
) -> BorrowedOwnedNativeProgramSourceRef<'a> {
    let base = index * 16;
    let row = admitted_borrowed_row(&wire[get_u32(rows, base)..]);
    let node_offset = get_u32(rows, base + 4);
    let leaf_offset = get_u32(rows, base + 8);
    BorrowedOwnedNativeProgramSourceRef {
        row,
        node: Some(wire[node_offset..node_offset + 32].try_into().unwrap()),
        program: wire[leaf_offset..leaf_offset + 32].try_into().unwrap(),
    }
}
fn charge_row<E>(
    wire: &[u8],
    rows: &[u8],
    index: usize,
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<(), E> {
    let offset = get_u32(rows, index * 16);
    let path_len = get_u32(wire, offset);
    let tag = offset + 4 + path_len + 32 + 4;
    let package_len = if wire[tag] == 1 {
        get_u32(wire, tag + 1)
    } else {
        0
    };
    for _ in 0..(path_len.saturating_add(package_len).saturating_add(4095) / 4096).max(1) {
        checkpoint(1)?;
    }
    Ok(())
}
fn sort_rows<E>(
    wire: &[u8],
    rows: &mut [u8],
    package: bool,
    checkpoint: &mut impl FnMut(u64) -> Result<(), E>,
) -> Result<(), E> {
    fn key<'a>(row: BorrowedOwnedNativeProgramSourceRef<'a>, package: bool) -> &'a [u8] {
        if package {
            row.package_path.unwrap_or(b"")
        } else {
            row.program_path
        }
    }
    fn swap(rows: &mut [u8], a: usize, b: usize) {
        for i in 0..16 {
            rows.swap(a * 16 + i, b * 16 + i);
        }
    }
    fn sift<E>(
        wire: &[u8],
        rows: &mut [u8],
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
            charge_row(wire, rows, child, checkpoint)?;
            if child + 1 < end {
                charge_row(wire, rows, child + 1, checkpoint)?;
            }
            if child + 1 < end
                && cmp(
                    key(row_at(wire, rows, child), package),
                    key(row_at(wire, rows, child + 1), package),
                    checkpoint,
                )?
                .is_lt()
            {
                child += 1;
            }
            charge_row(wire, rows, root, checkpoint)?;
            charge_row(wire, rows, child, checkpoint)?;
            if !cmp(
                key(row_at(wire, rows, root), package),
                key(row_at(wire, rows, child), package),
                checkpoint,
            )?
            .is_lt()
            {
                return Ok(());
            }
            swap(rows, root, child);
            root = child;
        }
    }
    let count = rows.len() / 16;
    for root in (0..count / 2).rev() {
        sift(wire, rows, root, count, package, checkpoint)?;
    }
    for end in (1..count).rev() {
        swap(rows, 0, end);
        sift(wire, rows, 0, end, package, checkpoint)?;
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
                let count = Leaf::scratch_bytes(leaf).map_err(Manifest)?;
                rows = rows
                    .checked_add(count / 8)
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
    /// Checkpoints charge a bounded record step or at most 4096 bytes of path/hash work.
    pub fn decode<E>(
        bytes: &'a [u8],
        nodes: &mut [u8],
        rows: &'s mut [u8],
        leaf_paths: &mut [u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Self, Error<E>> {
        use Error::{Manifest, Scratch, Stopped};
        let invalid = NativeProgramSourceManifestError::Encoding;
        let shape = Self::scratch(bytes, &mut checkpoint)?;
        if nodes.len() < shape.nodes * 8
            || rows.len() < shape.rows * 16
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
        let mut previous: Option<&[u8]> = None;
        let mut row_count = 0;
        for index in 0..count {
            checkpoint(1).map_err(Stopped)?;
            let path = input.cell().map_err(Manifest)?;
            validate_borrowed_path(path, &mut checkpoint)?;
            if let Some(p) = previous {
                if !cmp(p, path, &mut checkpoint).map_err(Stopped)?.is_lt() {
                    return Err(Manifest(invalid));
                }
            }
            previous = Some(path);
            let config = input.take(32).map_err(Manifest)?;
            ContentId::<SourceFactDomain>::try_from(config).map_err(|_| Manifest(invalid))?;
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
            for chunk in path.chunks(4096) {
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
                let node_offset = (member.as_ptr() as usize - bytes.as_ptr() as usize) - 36;
                let leaf_offset = member.as_ptr() as usize - bytes.as_ptr() as usize + 42;
                leaf.visit_sources(&mut checkpoint, |row| {
                    let row_offset =
                        row.program_path.as_ptr() as usize - bytes.as_ptr() as usize - 4;
                    put_u32(rows, row_count * 16, row_offset);
                    put_u32(rows, row_count * 16 + 4, node_offset);
                    put_u32(rows, row_count * 16 + 8, leaf_offset);
                    put_u32(rows, row_count * 16 + 12, 0);
                    row_count += 1;
                    Ok(())
                })
                .map_err(Stopped)?;
            }
            let reference_offset = references.as_ptr() as usize - bytes.as_ptr() as usize;
            put_u32(nodes, index * 8, reference_offset);
            put_u32(nodes, index * 8 + 4, references.len());
        }

        fn visit<E>(
            index: usize,
            nodes: &[u8],
            wire: &[u8],
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
            let start = get_u32(nodes, index * 8);
            let len = get_u32(nodes, index * 8 + 4);
            for reference in wire[start..start + len].chunks_exact(4) {
                visit(
                    u32::from_le_bytes(reference.try_into().unwrap()) as usize,
                    nodes,
                    wire,
                    state,
                    checkpoint,
                )?;
            }
            state[index] = 2;
            Ok(())
        }
        let mut state = [0; MAX_NATIVE_PROGRAM_NODES];
        visit(
            root,
            &nodes[..count * 8],
            bytes,
            &mut state,
            &mut checkpoint,
        )?;
        if project_set {
            for index in 0..count {
                visit(
                    index,
                    &nodes[..count * 8],
                    bytes,
                    &mut state,
                    &mut checkpoint,
                )?;
            }
        }
        if state[..count].iter().any(|s| *s != 2) {
            return Err(Manifest(invalid));
        }
        let rows = &mut rows[..row_count * 16];
        sort_rows(bytes, rows, false, &mut checkpoint).map_err(Stopped)?;
        let mut prior: Option<BorrowedOwnedNativeProgramSourceRef<'_>> = None;
        let mut owner = None;
        for index in 0..row_count {
            checkpoint(1).map_err(Stopped)?;
            charge_row(bytes, rows, index, &mut checkpoint).map_err(Stopped)?;
            let row = row_at(bytes, rows, index);
            if let Some(prev) = prior {
                if cmp(prev.program_path, row.program_path, &mut checkpoint)
                    .map_err(Stopped)?
                    .is_eq()
                {
                    let same_package = match (prev.package_path, row.package_path) {
                        (None, None) => true,
                        (Some(a), Some(b)) => cmp(a, b, &mut checkpoint).map_err(Stopped)?.is_eq(),
                        _ => false,
                    };
                    let owner_conflict = match (owner, row.package_path) {
                        (Some(a), Some(b)) => !cmp(a, b, &mut checkpoint).map_err(Stopped)?.is_eq(),
                        _ => false,
                    };
                    if prev.source != row.source
                        || (!project_set && !same_package)
                        || owner_conflict
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
        sort_rows(bytes, rows, true, &mut checkpoint).map_err(Stopped)?;
        for index in 1..row_count {
            charge_row(bytes, rows, index - 1, &mut checkpoint).map_err(Stopped)?;
            charge_row(bytes, rows, index, &mut checkpoint).map_err(Stopped)?;
            let (a, b) = (row_at(bytes, rows, index - 1), row_at(bytes, rows, index));
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
        let mut prior_image: Option<&[u8]> = None;
        for _ in 0..image_count {
            checkpoint(1).map_err(Stopped)?;
            let path = input.cell().map_err(Manifest)?;
            if let Some(previous) = prior_image {
                if !cmp(previous, path, &mut checkpoint)
                    .map_err(Stopped)?
                    .is_lt()
                {
                    return Err(Manifest(invalid));
                }
            }
            prior_image = Some(path);
            let recorded_node: [u8; 32] = input.take(32).map_err(Manifest)?.try_into().unwrap();
            let image_wire = input.take(32).map_err(Manifest)?;
            let image: [u8; 32] = image_wire.try_into().unwrap();
            SemanticImageIdentity::try_from(image).map_err(|_| Manifest(invalid))?;
            // Rows are sorted by package path. Binary search with metered comparisons.
            let mut low = 0;
            let mut high = row_count;
            while low < high {
                let mid = low + (high - low) / 2;
                charge_row(bytes, rows, mid, &mut checkpoint).map_err(Stopped)?;
                let key = row_at(bytes, rows, mid).package_path.unwrap_or(b"");
                if cmp(key, path, &mut checkpoint).map_err(Stopped)?.is_lt() {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            if low == row_count {
                return Err(Manifest(invalid));
            }
            charge_row(bytes, rows, low, &mut checkpoint).map_err(Stopped)?;
            let row = row_at(bytes, rows, low);
            let matching_path = match row.package_path {
                Some(value) => cmp(value, path, &mut checkpoint).map_err(Stopped)?.is_eq(),
                None => false,
            };
            if !matching_path || row.node != Some(recorded_node) {
                return Err(Manifest(invalid));
            }
            put_u32(
                rows,
                low * 16 + 12,
                image_wire.as_ptr() as usize - bytes.as_ptr() as usize,
            );
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
        mut visit: impl FnMut(BorrowedOwnedNativeProgramSourceRef<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        for index in 0..self.rows.len() / 16 {
            checkpoint(1)?;
            charge_row(self.bytes, self.rows, index, &mut checkpoint)?;
            visit(row_at(self.bytes, self.rows, index))?;
        }
        Ok(())
    }
    /// Verify a producer-sealed image against a UTF-8 path with metered lookup.
    pub fn image_matches<E>(
        &self,
        path: &str,
        image: &[u8; 32],
        checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        self.image_matches_bytes(path.as_bytes(), image, checkpoint)
    }
    /// Verify a producer-sealed image against borrowed path bytes with metered lookup.
    pub fn image_matches_bytes<E>(
        &self,
        path: &[u8],
        image: &[u8; 32],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut low = 0;
        let mut high = self.rows.len() / 16;
        while low < high {
            let mid = low + (high - low) / 2;
            charge_row(self.bytes, self.rows, mid, &mut checkpoint)?;
            let key = row_at(self.bytes, self.rows, mid)
                .package_path
                .unwrap_or(b"");
            if cmp(key, path, &mut checkpoint)?.is_lt() {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low == self.rows.len() / 16 {
            return Ok(false);
        }
        charge_row(self.bytes, self.rows, low, &mut checkpoint)?;
        if cmp(
            row_at(self.bytes, self.rows, low)
                .package_path
                .unwrap_or(b""),
            path,
            &mut checkpoint,
        )? != std::cmp::Ordering::Equal
        {
            return Ok(false);
        }
        let offset = get_u32(self.rows, low * 16 + 12);
        Ok(offset != 0 && self.bytes[offset..offset + 32] == *image)
    }

    /// Find a uniquely owned package source with metered binary lookup.
    pub fn source_for_package_bytes<E>(
        &self,
        path: &[u8],
        mut checkpoint: impl FnMut(u64) -> Result<(), E>,
    ) -> Result<Option<super::super::SourceIdentity>, E> {
        let mut low = 0;
        let mut high = self.rows.len() / 16;
        while low < high {
            let mid = low + (high - low) / 2;
            checkpoint(1)?;
            let key = row_at(self.bytes, self.rows, mid)
                .package_path
                .unwrap_or(b"");
            if cmp(key, path, &mut checkpoint)?.is_lt() {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low == self.rows.len() / 16 {
            return Ok(None);
        }
        checkpoint(1)?;
        let row = row_at(self.bytes, self.rows, low);
        match row.package_path {
            Some(candidate) if cmp(candidate, path, &mut checkpoint)?.is_eq() => {
                Ok(Some(row.source))
            }
            _ => Ok(None),
        }
    }
}
