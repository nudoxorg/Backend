//! Canonical `PSR*` source-record bytes.
//!
//! Restart admission re-encodes a persisted intent and refuses anything that
//! is not byte-identical. The tag is the shortest form that can carry the
//! row: `PSR8` states no containment, `PSR9` states where a declaration sits,
//! and `PSRA` also states the `SourceFactDomain` content identity.

use std::num::NonZeroU32;

use backend_compile::{
    Container, DeclarationKind, SourceDeclaration, SourceExcerpt, SourceExcerptExtent,
    SourceLanguage, SourceLocation,
};
use backend_version::{ContentId, SourceFactDomain};

use super::{
    DeclarationRetention, ProductSourceRecord, RetainedDeclarations, SourceUnavailableReason,
};

/// Canonical format tag for a record that states no containment.
pub(super) const SOURCE_RECORD_FORMAT_PLAIN: &[u8; 4] = b"PSR8";

/// Canonical format tag for a record that carries declaration containment.
const SOURCE_RECORD_FORMAT_CONTAINED: &[u8; 4] = b"PSR9";

/// Canonical format tag for a record that also states its semantic source
/// content identity.
pub(super) const SOURCE_RECORD_FORMAT_IDENTIFIED: &[u8; 4] = b"PSRA";

/// Newest source-record format version, as the tags above name it.
const SOURCE_RECORD_VERSION: u8 = 10;

/// Returns the lowest format tag that can carry this record.
///
/// A persisted intent embeds these exact bytes, and restart admission
/// decodes and re-encodes them and refuses anything not byte-identical. A
/// record with nothing new to say must therefore still encode exactly the
/// way it always did, or every workspace written before containment existed
/// would stop opening. The tag is minimal for that reason, the way a
/// canonical integer takes the shortest form: `PSR9` means precisely "at
/// least one declaration states where it sits", and `PSRA` means "the row
/// also states which `SourceFactDomain` content identity its exact bytes
/// hash to".
///
/// Decoding stays liberal - it admits a `PSR9` record that states no
/// containment and normalizes it to `PSR8` on the way out - because refusing
/// a record whose bytes are perfectly readable would make a whole workspace
/// unopenable over a tag.
pub(super) fn source_record_format(value: &ProductSourceRecord) -> &'static [u8; 4] {
    match value {
        ProductSourceRecord::Project { .. } => SOURCE_RECORD_FORMAT_PLAIN,
        ProductSourceRecord::File {
            declarations,
            source_identity,
            ..
        } => {
            if source_identity.is_some() {
                SOURCE_RECORD_FORMAT_IDENTIFIED
            } else if states_containment(declarations) {
                SOURCE_RECORD_FORMAT_CONTAINED
            } else {
                SOURCE_RECORD_FORMAT_PLAIN
            }
        }
    }
}

/// Returns whether any declaration sits anywhere but its file module.
fn states_containment(declarations: &[SourceDeclaration]) -> bool {
    declarations
        .iter()
        .any(|declaration| !matches!(declaration.container(), Container::Module))
}

/// Returns the version of an admitted `PSR*` format tag.
fn format_version(format: &[u8]) -> Option<u8> {
    let [b'P', b'S', b'R', tag] = format else {
        return None;
    };
    // `PSRA` is the identified format: the eleventh shape, named by a letter
    // because a tenth digit would read as "1" followed by nothing.
    let version = match tag {
        b'A' => 10,
        digit => digit.checked_sub(b'0')?,
    };
    (2..=SOURCE_RECORD_VERSION)
        .contains(&version)
        .then_some(version)
}

/// Encodes one file record body after the format tag.
pub(super) fn encode_file_record(value: &ProductSourceRecord, output: &mut Vec<u8>) {
    let ProductSourceRecord::File {
        project,
        path,
        language,
        content_version,
        analysis_version,
        source_identity,
        declarations,
        retention,
    } = value
    else {
        return;
    };
    let identified = source_identity.is_some();
    debug_assert_eq!(
        identified,
        source_record_format(value) == SOURCE_RECORD_FORMAT_IDENTIFIED,
        "the minimal format tag must name the identity exactly when one is stated"
    );
    let contained = source_record_format(value) == SOURCE_RECORD_FORMAT_CONTAINED;
    output.push(2);
    output.extend_from_slice(project);
    push_text(output, path);
    output.push(language.wire_tag());
    output.extend_from_slice(content_version);
    output.extend_from_slice(analysis_version);
    if identified {
        output.extend_from_slice(source_identity.as_ref().expect("checked above").as_ref());
    }
    push_retention(output, *retention);
    push_count(output, declarations.len());
    for declaration in declarations.iter() {
        output.extend_from_slice(&declaration.line().to_be_bytes());
        // The file relation remains the authority for the selected path.
        // Retaining a declaration's typed location is useful for callers, but
        // an inconsistent path must not silently alter the file identity.
        push_text(output, path);
        push_text(output, declaration.name());
        output.push(declaration.kind().wire_tag());
        push_text(output, declaration.signature());
        push_text(output, declaration.documentation());
        push_excerpt(output, declaration.source_excerpt());
        if identified {
            // The identified format carries containment unconditionally: its
            // tag already spends the format discriminant on the identity, so
            // containment cannot also select the tag.
            push_container(output, declaration.container());
        } else if contained {
            push_container(output, declaration.container());
        }
    }
}

/// Encodes one declaration's extracted containment.
fn push_container(output: &mut Vec<u8>, container: &Container) {
    match container {
        Container::Module => output.push(0),
        Container::Enclosing { name, line } => {
            output.push(1);
            push_text(output, name);
            output.extend_from_slice(&line.get().to_be_bytes());
        }
        Container::Attached { type_name } => {
            output.push(2);
            push_text(output, type_name);
        }
    }
}

/// Encodes one declaration excerpt, including its typed absence markers.
fn push_excerpt(output: &mut Vec<u8>, excerpt: &SourceExcerpt) {
    match excerpt {
        SourceExcerpt::NotCaptured => output.push(0),
        SourceExcerpt::Captured {
            text,
            extent: SourceExcerptExtent::Complete,
        } => {
            output.push(1);
            push_text(output, text);
        }
        SourceExcerpt::Captured {
            text,
            extent: SourceExcerptExtent::Truncated,
        } => {
            output.push(2);
            push_text(output, text);
        }
        SourceExcerpt::NotHydrated => output.push(3),
        SourceExcerpt::Unconfigured => output.push(4),
    }
}

pub(super) fn push_count(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
}

fn push_retention(output: &mut Vec<u8>, retention: DeclarationRetention) {
    match retention {
        DeclarationRetention::Complete => output.push(0),
        DeclarationRetention::ExcerptsElided => output.push(1),
        DeclarationRetention::NamesOnly => output.push(2),
        DeclarationRetention::Truncated(counts) => {
            output.push(3);
            output.extend_from_slice(&counts.retained().to_be_bytes());
            output.extend_from_slice(&counts.extracted().to_be_bytes());
        }
        DeclarationRetention::Unavailable(reason) => {
            output.push(4);
            output.push(match reason {
                SourceUnavailableReason::Unreadable => 0,
                SourceUnavailableReason::NotText => 1,
                SourceUnavailableReason::TooLarge => 2,
                SourceUnavailableReason::Unparsed => 3,
            });
        }
    }
}

pub(super) fn push_text(output: &mut Vec<u8>, value: &str) {
    push_count(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

pub(super) fn decode_source_record(bytes: &[u8]) -> Result<ProductSourceRecord, ()> {
    let mut reader = SourceReader::new(bytes);
    let version = format_version(reader.take(4)?).ok_or(())?;
    let record = match reader.byte()? {
        1 => {
            let label = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let source_version = reader.array()?;
            let count = reader.count(ProductSourceRecord::MAX_PROJECT_FILES)?;
            let mut files = Vec::with_capacity(count);
            for _ in 0..count {
                files.push(reader.array()?);
            }
            ProductSourceRecord::project(label, source_version, files).map_err(|_| ())?
        }
        2 => decode_file_record(&mut reader, version)?,
        _ => return Err(()),
    };
    reader.finish()?;
    Ok(record)
}

/// Decodes one file record body for any admitted `PSR*` format.
fn decode_file_record(
    reader: &mut SourceReader<'_>,
    version: u8,
) -> Result<ProductSourceRecord, ()> {
    let project = reader.array()?;
    let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
    let language = SourceLanguage::from_wire_tag(reader.byte()?).ok_or(())?;
    let content_version = reader.array()?;
    let analysis_version = if version >= 3 {
        reader.array()?
    } else {
        [0; 32]
    };
    let source_identity = if version >= 10 {
        Some(ContentId::<SourceFactDomain>::try_from(reader.array()?).map_err(|_| ())?)
    } else {
        None
    };
    let retention = if version >= 8 {
        decode_retention(reader)?
    } else {
        DeclarationRetention::Complete
    };
    let count = reader.count(ProductSourceRecord::MAX_FILE_DECLARATIONS)?;
    // Containment is tag-selected: `PSR9` and `PSRA` both carry it, one per
    // declaration, while every earlier format implies the file module.
    let contained = version >= 9;
    let mut declarations = Vec::with_capacity(count);
    for _ in 0..count {
        declarations.push(decode_declaration(reader, version, &path, contained)?);
    }
    if version == 6 {
        skip_legacy_semantics(reader)?;
    }
    let record = ProductSourceRecord::file_with_retention(
        project,
        path,
        language,
        content_version,
        analysis_version,
        declarations,
        retention,
    )
    .map_err(|_| ())?;
    match source_identity {
        Some(identity) if matches!(record, ProductSourceRecord::File { .. }) => {
            record.with_source_identity(identity).map_err(|_| ())
        }
        _ => Ok(record),
    }
}

/// Decodes one declaration, tolerating the older pre-`PSR4` field shapes.
fn decode_declaration(
    reader: &mut SourceReader<'_>,
    version: u8,
    path: &str,
    contained: bool,
) -> Result<SourceDeclaration, ()> {
    let line = reader.u32()?;
    let declaration_path = if version >= 4 {
        reader.text(SourceLocation::MAX_PATH_BYTES)?
    } else {
        path.to_owned()
    };
    if declaration_path != path {
        return Err(());
    }
    let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let kind = if version >= 4 {
        DeclarationKind::from_wire_tag(reader.byte()?).ok_or(())?
    } else {
        DeclarationKind::from_name(&reader.text(SourceDeclaration::MAX_TEXT_BYTES)?)
    };
    let signature = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let documentation = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
    let source_excerpt = if version >= 5 {
        decode_source_excerpt(reader)?
    } else {
        SourceExcerpt::NotCaptured
    };
    let decoded_container = if contained {
        decode_container(reader)?
    } else {
        Container::Module
    };
    Ok(SourceDeclaration::with_location(
        SourceLocation::new(declaration_path, line).map_err(|_| ())?,
        name,
        kind.name(),
        signature,
        documentation,
    )
    .map_err(|_| ())?
    .with_source_excerpt(source_excerpt)
    .with_container(decoded_container))
}
/// Decodes one declaration's containment, rejecting an unknown discriminant.
fn decode_container(reader: &mut SourceReader<'_>) -> Result<Container, ()> {
    match reader.byte()? {
        0 => Ok(Container::Module),
        1 => {
            let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
            let line = NonZeroU32::new(reader.u32()?).ok_or(())?;
            Ok(Container::enclosing(&name, line))
        }
        2 => Ok(Container::attached(
            &reader.text(SourceDeclaration::MAX_TEXT_BYTES)?,
        )),
        _ => Err(()),
    }
}

fn skip_legacy_semantics(reader: &mut SourceReader<'_>) -> Result<(), ()> {
    let tag = reader.byte()?;
    if matches!(tag, 0 | 1) {
        return Ok(());
    }
    if !matches!(tag, 2 | 3) {
        return Err(());
    }
    let _: [u8; 32] = reader.array()?;
    let _: [u8; 32] = reader.array()?;
    let _ = reader.u64()?;
    let count = reader.count(backend_compile::MAX_NATIVE_RECORDS)?;
    for _ in 0..count {
        if !matches!(reader.byte()?, 1..=6) {
            return Err(());
        }
        let _ = reader.bytes(backend_compile::MAX_NATIVE_KEY_BYTES)?;
        let _ = reader.bytes(backend_compile::MAX_NATIVE_VALUE_BYTES)?;
    }
    Ok(())
}

fn decode_retention(reader: &mut SourceReader<'_>) -> Result<DeclarationRetention, ()> {
    match reader.byte()? {
        0 => Ok(DeclarationRetention::Complete),
        1 => Ok(DeclarationRetention::ExcerptsElided),
        2 => Ok(DeclarationRetention::NamesOnly),
        3 => {
            let retained = reader.u32()?;
            let extracted = reader.u32()?;
            RetainedDeclarations::new(retained, extracted)
                .map(DeclarationRetention::Truncated)
                .map_err(|_| ())
        }
        4 => match reader.byte()? {
            0 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unreadable,
            )),
            1 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText,
            )),
            2 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::TooLarge,
            )),
            3 => Ok(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unparsed,
            )),
            _ => Err(()),
        },
        _ => Err(()),
    }
}

fn decode_source_excerpt(reader: &mut SourceReader<'_>) -> Result<SourceExcerpt, ()> {
    let tag = reader.byte()?;
    match tag {
        0 => Ok(SourceExcerpt::NotCaptured),
        1 | 2 => {
            let text = reader.text(SourceExcerpt::MAX_BYTES)?;
            SourceExcerpt::captured(
                &text,
                if tag == 1 {
                    SourceExcerptExtent::Complete
                } else {
                    SourceExcerptExtent::Truncated
                },
            )
            .map_err(|_| ())
        }
        3 => Ok(SourceExcerpt::NotHydrated),
        4 => Ok(SourceExcerpt::Unconfigured),
        _ => Err(()),
    }
}

struct SourceReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> SourceReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ()> {
        let end = self.at.checked_add(length).ok_or(())?;
        let value = self.bytes.get(self.at..end).ok_or(())?;
        self.at = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ()> {
        self.take(1)?.first().copied().ok_or(())
    }

    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| ())?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| ())?,
        ))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, ()> {
        let count = self.u32()? as usize;
        (count <= maximum).then_some(count).ok_or(())
    }

    fn array(&mut self) -> Result<[u8; 32], ()> {
        self.take(32)?.try_into().map_err(|_| ())
    }

    fn text(&mut self, maximum: usize) -> Result<String, ()> {
        let length = self.count(maximum)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| ())
    }

    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], ()> {
        let length = self.count(maximum)?;
        self.take(length)
    }

    fn finish(self) -> Result<(), ()> {
        (self.at == self.bytes.len()).then_some(()).ok_or(())
    }
}
