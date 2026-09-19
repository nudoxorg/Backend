//! Allocation-free output mechanics and canonical byte spelling.

use core::fmt;

use crate::ir::{AtomId, SemanticReader, TypeId};

use super::{CanonicalTypeRenderError, CanonicalTypeRenderReference};

pub(super) fn tagged_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    tag: &str,
    atom: AtomId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, tag)?;
    write_text(root, output, "(")?;
    emit_atom(reader, root, owner, atom, output)?;
    write_text(root, output, ")")
}

pub(super) fn emit_optional_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    atom: Option<AtomId>,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    match atom {
        Some(atom) => emit_atom(reader, root, owner, atom, output),
        None => write_text(root, output, "none"),
    }
}

pub(super) fn emit_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    root: TypeId,
    owner: TypeId,
    atom: AtomId,
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    let bytes = reader
        .atom(atom)
        .ok_or(CanonicalTypeRenderError::MissingReference {
            owner,
            reference: CanonicalTypeRenderReference::Atom(atom),
        })?;
    emit_bytes(root, bytes, output)
}

pub(super) fn emit_bytes(
    root: TypeId,
    bytes: &[u8],
    output: &mut impl fmt::Write,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, "x\"")?;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in bytes {
        write_char(root, output, char::from(HEX[usize::from(*byte >> 4)]))?;
        write_char(root, output, char::from(HEX[usize::from(*byte & 0x0F)]))?;
    }
    write_text(root, output, "\"")
}

pub(super) fn tagged_text(
    root: TypeId,
    output: &mut impl fmt::Write,
    tag: &str,
    value: &str,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, tag)?;
    write_text(root, output, "(")?;
    write_text(root, output, value)?;
    write_text(root, output, ")")
}

pub(super) fn write_bool(
    root: TypeId,
    output: &mut impl fmt::Write,
    value: bool,
) -> Result<(), CanonicalTypeRenderError> {
    write_text(root, output, if value { "true" } else { "false" })
}

pub(super) fn write_number(
    root: TypeId,
    output: &mut impl fmt::Write,
    value: u64,
) -> Result<(), CanonicalTypeRenderError> {
    fmt::write(output, format_args!("{value}"))
        .map_err(|_| CanonicalTypeRenderError::OutputWrite { root })
}

pub(super) fn write_text(
    root: TypeId,
    output: &mut impl fmt::Write,
    value: &str,
) -> Result<(), CanonicalTypeRenderError> {
    output
        .write_str(value)
        .map_err(|_| CanonicalTypeRenderError::OutputWrite { root })
}

fn write_char(
    root: TypeId,
    output: &mut impl fmt::Write,
    value: char,
) -> Result<(), CanonicalTypeRenderError> {
    output
        .write_char(value)
        .map_err(|_| CanonicalTypeRenderError::OutputWrite { root })
}

pub(super) struct CountWriter {
    encoded_len: Option<usize>,
}

impl CountWriter {
    pub(super) const fn new() -> Self {
        Self {
            encoded_len: Some(0),
        }
    }

    pub(super) const fn encoded_len(&self) -> Option<usize> {
        self.encoded_len
    }
}

impl fmt::Write for CountWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.encoded_len = self
            .encoded_len
            .and_then(|length| length.checked_add(value.len()));
        Ok(())
    }
}

pub(super) struct ByteWriter<'output> {
    output: &'output mut [u8],
    written: usize,
}

impl<'output> ByteWriter<'output> {
    pub(super) const fn new(output: &'output mut [u8]) -> Self {
        Self { output, written: 0 }
    }

    pub(super) const fn written_len(&self) -> usize {
        self.written
    }
}

impl fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.output.get_mut(self.written..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
