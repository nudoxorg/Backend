//! Defines source behavior for `interface-cli`, whose purpose is to serve the unified application protocol over a command-line process.
//! This module owns the source invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded file and stream ownership for compiler source requests.

use std::{
    fs::File,
    io::{self, Read},
};

use interface_core::{ApplicationInput, GenerateTarget, PORTABLE_LOCAL_SOURCE_LIMIT};
use interface_protocol::{
    AdapterError, CliCommand, SourceIngressPhase, SourceIngressRole, SourceIoFact, source_input,
};

/// Stack buffer used by the CLI's bounded source ingress reader.
const SOURCE_READ_CHUNK_BYTES: usize = 8 * 1024;
/// First earned allocation for a source stream with no reliable length metadata.
const SOURCE_STREAM_INITIAL_BYTES: usize = SOURCE_READ_CHUNK_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceCapacityPlan {
    FileMetadata(usize),
    Stream,
}

pub(crate) fn application_input(
    command: CliCommand,
    standard_input_consumed: &mut bool,
) -> Result<ApplicationInput, AdapterError> {
    match command {
        CliCommand::Application(input) => Ok(input),
        CliCommand::GenerateFile { target, path } => {
            let mut file = File::open(path).map_err(|source| {
                source_io(
                    SourceIngressRole::FilePath,
                    SourceIngressPhase::Open,
                    &source,
                )
            })?;
            let metadata = file.metadata().map_err(|source| {
                source_io(
                    SourceIngressRole::FilePath,
                    SourceIngressPhase::Metadata,
                    &source,
                )
            })?;
            source_from_reader(
                target,
                &mut file,
                SourceIngressRole::FilePath,
                source_file_capacity_plan(metadata.len()),
            )
        }
        CliCommand::GenerateStandardInput { target } => {
            if *standard_input_consumed {
                return Err(AdapterError::standard_input_consumed());
            }
            *standard_input_consumed = true;
            let standard_input = io::stdin();
            source_from_reader(
                target,
                &mut standard_input.lock(),
                SourceIngressRole::StandardInput,
                SourceCapacityPlan::Stream,
            )
        }
    }
}

fn source_from_reader<Reader: Read>(
    target: GenerateTarget,
    reader: &mut Reader,
    role: SourceIngressRole,
    capacity_plan: SourceCapacityPlan,
) -> Result<ApplicationInput, AdapterError> {
    source_input(target, read_bounded_utf8(reader, role, capacity_plan)?)
}

fn source_file_capacity_plan(metadata_bytes: u64) -> SourceCapacityPlan {
    match usize::try_from(metadata_bytes) {
        Ok(bytes) if bytes <= PORTABLE_LOCAL_SOURCE_LIMIT.bytes => {
            SourceCapacityPlan::FileMetadata(bytes)
        }
        Ok(_) | Err(_) => SourceCapacityPlan::Stream,
    }
}

fn read_bounded_utf8<Reader: Read>(
    reader: &mut Reader,
    role: SourceIngressRole,
    capacity_plan: SourceCapacityPlan,
) -> Result<String, AdapterError> {
    let maximum_read = PORTABLE_LOCAL_SOURCE_LIMIT
        .bytes
        .checked_add(1)
        .ok_or_else(|| AdapterError::source_limit_overflow(PORTABLE_LOCAL_SOURCE_LIMIT))?;
    let initial_capacity = initial_source_capacity(capacity_plan, maximum_read);
    let mut bytes = Vec::new();
    if initial_capacity != 0 {
        bytes
            .try_reserve_exact(initial_capacity)
            .map_err(|source| AdapterError::source_allocation(role, initial_capacity, source))?;
    }
    let mut chunk = [0_u8; SOURCE_READ_CHUNK_BYTES];

    while bytes.len() < maximum_read {
        let requested = (maximum_read - bytes.len()).min(chunk.len());
        let read = reader
            .read(&mut chunk[..requested])
            .map_err(|source| source_io(role, SourceIngressPhase::Read, &source))?;
        if read == 0 {
            break;
        }
        reserve_source_append(&mut bytes, read, maximum_read, role)?;
        bytes.extend_from_slice(&chunk[..read]);
    }

    String::from_utf8(bytes).map_err(|source| {
        let utf8 = source.utf8_error();
        AdapterError::source_encoding(source.into_bytes(), utf8)
    })
}

fn initial_source_capacity(plan: SourceCapacityPlan, maximum_read: usize) -> usize {
    match plan {
        SourceCapacityPlan::FileMetadata(bytes) => bytes,
        SourceCapacityPlan::Stream => SOURCE_STREAM_INITIAL_BYTES,
    }
    .min(maximum_read)
}

fn reserve_source_append(
    bytes: &mut Vec<u8>,
    appended: usize,
    maximum_read: usize,
    role: SourceIngressRole,
) -> Result<(), AdapterError> {
    let required = bytes
        .len()
        .checked_add(appended)
        .ok_or_else(|| AdapterError::source_capacity_overflow(bytes.len(), appended))?;
    if required <= bytes.capacity() {
        return Ok(());
    }
    let doubled = match bytes.capacity().checked_mul(2) {
        Some(capacity) => capacity,
        None => maximum_read,
    };
    let target_capacity = doubled.max(required).min(maximum_read);
    let additional = target_capacity - bytes.len();
    bytes
        .try_reserve_exact(additional)
        .map_err(|source| AdapterError::source_allocation(role, additional, source))
}

fn source_io(
    role: SourceIngressRole,
    phase: SourceIngressPhase,
    source: &io::Error,
) -> AdapterError {
    AdapterError::source_io(SourceIoFact {
        role,
        phase,
        kind: source.kind(),
        raw_os_code: source.raw_os_error(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};

    use compiler_vocabulary::{Language, Stage};
    use interface_core::{ApplicationInput, CorrelationId, GenerateTarget, RejectedSourceText};
    use interface_protocol::AdapterErrorCause;

    use super::{
        PORTABLE_LOCAL_SOURCE_LIMIT, SOURCE_READ_CHUNK_BYTES, SOURCE_STREAM_INITIAL_BYTES,
        SourceCapacityPlan, SourceIngressRole, initial_source_capacity, reserve_source_append,
        source_from_reader,
    };

    struct MeasuredReader {
        bytes: Cursor<Vec<u8>>,
        largest_request: usize,
    }

    impl MeasuredReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes: Cursor::new(bytes),
                largest_request: 0,
            }
        }
    }

    impl Read for MeasuredReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.largest_request = self.largest_request.max(buffer.len());
            self.bytes.read(buffer)
        }
    }

    fn target() -> GenerateTarget {
        GenerateTarget {
            correlation: CorrelationId(41),
            language: Language::Rust,
            stage: Stage::LowerIr,
        }
    }

    #[test]
    fn readers_stop_at_the_named_plus_one_sentinel() -> Result<(), io::Error> {
        let mut file = MeasuredReader::new(vec![b'x'; PORTABLE_LOCAL_SOURCE_LIMIT.bytes]);
        let ApplicationInput::Generate(request) = source_from_reader(
            target(),
            &mut file,
            SourceIngressRole::FilePath,
            SourceCapacityPlan::FileMetadata(PORTABLE_LOCAL_SOURCE_LIMIT.bytes),
        )
        .map_err(io::Error::other)?
        else {
            return Err(io::Error::other(
                "file source decoded to a non-generate input",
            ));
        };
        assert_eq!(request.source.len(), PORTABLE_LOCAL_SOURCE_LIMIT.bytes);

        let mut input = MeasuredReader::new(vec![b'x'; PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 2]);
        let Err(error) = source_from_reader(
            target(),
            &mut input,
            SourceIngressRole::StandardInput,
            SourceCapacityPlan::Stream,
        ) else {
            return Err(io::Error::other("oversize standard input was admitted"));
        };
        assert!(matches!(
            error.cause,
            Some(AdapterErrorCause::SourceLength(RejectedSourceText { source, error }))
                if source.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
                    && error.observed == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
        ));
        assert_eq!(
            input.bytes.position(),
            u64::try_from(PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1).map_err(io::Error::other)?,
        );
        assert!(file.largest_request <= SOURCE_READ_CHUNK_BYTES);
        assert!(input.largest_request <= SOURCE_READ_CHUNK_BYTES);
        Ok(())
    }

    #[test]
    fn invalid_utf8_retains_only_bounded_input() -> Result<(), io::Error> {
        let mut bytes = vec![b'x'; PORTABLE_LOCAL_SOURCE_LIMIT.bytes];
        bytes.extend([0xff, b'y']);
        let mut reader = MeasuredReader::new(bytes);
        let Err(error) = source_from_reader(
            target(),
            &mut reader,
            SourceIngressRole::FilePath,
            SourceCapacityPlan::FileMetadata(PORTABLE_LOCAL_SOURCE_LIMIT.bytes),
        ) else {
            return Err(io::Error::other("invalid UTF-8 source was admitted"));
        };
        assert!(matches!(
            error.cause,
            Some(AdapterErrorCause::SourceEncoding(error))
                if error.bytes.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
                    && error.valid_up_to == PORTABLE_LOCAL_SOURCE_LIMIT.bytes
                    && error.error_length.is_some()
        ));
        assert_eq!(
            reader.bytes.position(),
            u64::try_from(PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1).map_err(io::Error::other)?,
        );
        Ok(())
    }

    #[test]
    fn file_hint_and_stream_growth_are_bounded() -> Result<(), io::Error> {
        const MAX_GEOMETRIC_TRANSITIONS: u8 = 8;
        let maximum = PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1;
        let mut file = Vec::new();
        let initial = initial_source_capacity(
            SourceCapacityPlan::FileMetadata(PORTABLE_LOCAL_SOURCE_LIMIT.bytes),
            maximum,
        );
        file.try_reserve_exact(initial).map_err(io::Error::other)?;
        let file_capacity = file.capacity();
        reserve_source_append(&mut file, initial, maximum, SourceIngressRole::FilePath)
            .map_err(io::Error::other)?;
        assert_eq!(file.capacity(), file_capacity);

        let mut stream = Vec::new();
        stream
            .try_reserve_exact(SOURCE_STREAM_INITIAL_BYTES)
            .map_err(io::Error::other)?;
        let mut capacity_changes = 1_u8;
        while stream.len() < PORTABLE_LOCAL_SOURCE_LIMIT.bytes {
            let appended =
                SOURCE_READ_CHUNK_BYTES.min(PORTABLE_LOCAL_SOURCE_LIMIT.bytes - stream.len());
            let before = stream.capacity();
            reserve_source_append(
                &mut stream,
                appended,
                maximum,
                SourceIngressRole::StandardInput,
            )
            .map_err(io::Error::other)?;
            capacity_changes += u8::from(stream.capacity() != before);
            stream.extend(core::iter::repeat_n(b'x', appended));
        }
        assert!(capacity_changes <= MAX_GEOMETRIC_TRANSITIONS);
        Ok(())
    }
}
