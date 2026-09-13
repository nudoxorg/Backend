//! Measures `heart-root` benches capacity-planning runner fixture work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Bounded source, compact-fragment, and durable-path fixtures for one sample.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use compiler_driver::CompiledFragment;
use backend_semantic::ir::FragmentView;
use server_journal::PublicationPaths;

use crate::{BenchmarkError, model::MAX_CORPUS};

const SOURCE_BYTES: usize = 64;
const FRAGMENT_BYTES: usize = 512;
static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct SourceSlot {
    bytes: [u8; SOURCE_BYTES],
    len: usize,
}

impl SourceSlot {
    const EMPTY: Self = Self {
        bytes: [0; SOURCE_BYTES],
        len: 0,
    };

    fn source(&self) -> Result<&[u8], BenchmarkError> {
        self.bytes
            .get(..self.len)
            .ok_or(BenchmarkError::SourceSlotLength { len: self.len })
    }

    fn name(&self) -> Result<&[u8], BenchmarkError> {
        self.bytes
            .get(10..17)
            .ok_or(BenchmarkError::SourceSlotLength { len: self.len })
    }
}

pub(crate) struct Corpus {
    slots: [SourceSlot; MAX_CORPUS],
    pub(crate) len: usize,
}

impl Corpus {
    pub(crate) fn new(len: usize) -> Result<Self, BenchmarkError> {
        if len == 0 || len > MAX_CORPUS {
            return Err(BenchmarkError::CorpusSize {
                maximum: MAX_CORPUS,
                observed: len,
            });
        }
        let mut slots = [SourceSlot::EMPTY; MAX_CORPUS];
        for (index, slot) in slots.iter_mut().take(len).enumerate() {
            *slot = source_slot(index)?;
        }
        Ok(Self { slots, len })
    }

    fn slot(&self, index: usize) -> Result<&SourceSlot, BenchmarkError> {
        self.slots
            .get(index)
            .filter(|_slot| index < self.len)
            .ok_or(BenchmarkError::CorpusSlot {
                index,
                len: self.len,
            })
    }

    pub(crate) fn source(&self, index: usize) -> Result<&[u8], BenchmarkError> {
        self.slot(index)?.source()
    }

    pub(crate) fn name(&self, index: usize) -> Result<&[u8], BenchmarkError> {
        self.slot(index)?.name()
    }

    pub(crate) fn byte_len(&self) -> Result<u64, BenchmarkError> {
        self.slots
            .iter()
            .take(self.len)
            .try_fold(0_u64, |total, slot| {
                let width = u64::try_from(slot.len).map_err(BenchmarkError::ByteCount)?;
                total
                    .checked_add(width)
                    .ok_or(BenchmarkError::ByteCountOverflow)
            })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FragmentSlot {
    pub(crate) bytes: [u8; FRAGMENT_BYTES],
    pub(crate) len: usize,
}

impl FragmentSlot {
    const EMPTY: Self = Self {
        bytes: [0; FRAGMENT_BYTES],
        len: 0,
    };
}

pub(crate) struct FragmentSlots {
    pub(crate) slots: [FragmentSlot; MAX_CORPUS],
}

impl FragmentSlots {
    #[allow(
        clippy::large_stack_arrays,
        reason = "the bounded caller-owned IR buffer avoids per-fragment heap allocation and is part of the capacity measurement"
    )]
    pub(crate) const fn new() -> Self {
        Self {
            slots: [FragmentSlot::EMPTY; MAX_CORPUS],
        }
    }

    pub(crate) fn compiled(
        &self,
        count: usize,
    ) -> Result<Vec<CompiledFragment<'_>>, BenchmarkError> {
        let mut compiled = Vec::with_capacity(count);
        for (index, slot) in self.slots.iter().take(count).enumerate() {
            let bytes = slot
                .bytes
                .get(..slot.len)
                .ok_or(BenchmarkError::FragmentSlot {
                    index,
                    len: slot.len,
                })?;
            let fragment = FragmentView::validate(bytes)
                .map_err(|source| BenchmarkError::FragmentValidate(Box::new(source)))?;
            compiled.push(CompiledFragment {
                source: fragment.source,
                recipe: fragment.recipe,
                fragment,
            });
        }
        Ok(compiled)
    }

    pub(crate) fn bytes(&self, index: usize) -> Result<&[u8], BenchmarkError> {
        let slot = self.slots.get(index).ok_or(BenchmarkError::FragmentIndex {
            index,
            capacity: self.slots.len(),
        })?;
        slot.bytes
            .get(..slot.len)
            .ok_or(BenchmarkError::FragmentSlot {
                index,
                len: slot.len,
            })
    }
}

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) native_work: PathBuf,
}

impl Fixture {
    pub(crate) fn create(base: &Path, kind: &str, ordinal: usize) -> Result<Self, BenchmarkError> {
        fs::create_dir_all(base).map_err(|source| BenchmarkError::CreateDirectory {
            path: base.to_path_buf(),
            source,
        })?;
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(BenchmarkError::Clock)?
            .as_nanos();
        let root = base.join(format!(
            "{kind}-{}-{ordinal}-{sequence}-{epoch}",
            std::process::id()
        ));
        fs::create_dir(&root).map_err(|source| BenchmarkError::CreateDirectory {
            path: root.clone(),
            source,
        })?;
        let native_work = root.join("native-work");
        fs::create_dir(&native_work).map_err(|source| BenchmarkError::CreateDirectory {
            path: native_work.clone(),
            source,
        })?;
        Ok(Self { root, native_work })
    }

    pub(crate) fn artifacts(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub(crate) fn journal(&self) -> PublicationPaths {
        PublicationPaths::in_directory(&self.root.join("journal"))
    }
}

fn source_slot(index: usize) -> Result<SourceSlot, BenchmarkError> {
    let mut bytes = [0_u8; SOURCE_BYTES];
    let mut cursor = 0;
    append(&mut bytes, &mut cursor, b"pub const ")?;
    append(&mut bytes, &mut cursor, b"item")?;
    append_padded_decimal(&mut bytes, &mut cursor, index, 3)?;
    append(&mut bytes, &mut cursor, b": i32 = ")?;
    append_decimal(&mut bytes, &mut cursor, index)?;
    append(&mut bytes, &mut cursor, b";")?;
    Ok(SourceSlot { bytes, len: cursor })
}

fn append(output: &mut [u8], cursor: &mut usize, input: &[u8]) -> Result<(), BenchmarkError> {
    let end = cursor
        .checked_add(input.len())
        .ok_or(BenchmarkError::SourceCapacity)?;
    let destination = output
        .get_mut(*cursor..end)
        .ok_or(BenchmarkError::SourceCapacity)?;
    destination.copy_from_slice(input);
    *cursor = end;
    Ok(())
}

fn append_padded_decimal(
    output: &mut [u8],
    cursor: &mut usize,
    value: usize,
    width: usize,
) -> Result<(), BenchmarkError> {
    let end = cursor
        .checked_add(width)
        .ok_or(BenchmarkError::SourceCapacity)?;
    let destination = output
        .get_mut(*cursor..end)
        .ok_or(BenchmarkError::SourceCapacity)?;
    let mut divisor = 1_usize;
    for _ in 1..width {
        divisor = divisor
            .checked_mul(10)
            .ok_or(BenchmarkError::SourceCapacity)?;
    }
    let mut remaining = value;
    for slot in destination {
        let digit = remaining / divisor;
        let digit = u8::try_from(digit).map_err(BenchmarkError::ByteCount)?;
        *slot = b'0'
            .checked_add(digit)
            .ok_or(BenchmarkError::SourceCapacity)?;
        remaining %= divisor;
        divisor /= 10;
    }
    *cursor = end;
    Ok(())
}

fn append_decimal(
    output: &mut [u8],
    cursor: &mut usize,
    value: usize,
) -> Result<(), BenchmarkError> {
    if value == 0 {
        return append(output, cursor, b"0");
    }
    let mut scratch = [0_u8; 20];
    let mut remaining = value;
    let mut width = 0;
    while remaining > 0 {
        let digit = u8::try_from(remaining % 10).map_err(BenchmarkError::ByteCount)?;
        let slot = scratch
            .get_mut(width)
            .ok_or(BenchmarkError::SourceCapacity)?;
        *slot = b'0'
            .checked_add(digit)
            .ok_or(BenchmarkError::SourceCapacity)?;
        width += 1;
        remaining /= 10;
    }
    let digits = scratch.get(..width).ok_or(BenchmarkError::SourceCapacity)?;
    for digit in digits.iter().rev().copied() {
        append(output, cursor, &[digit])?;
    }
    Ok(())
}
