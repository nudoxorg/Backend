use super::*;

/// Archive encoding supplied by a forge transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeArchiveFormat {
    /// POSIX tar, optionally gzip-compressed by the transport.
    Tar,
    /// Gzip-compressed POSIX tar.
    TarGzip,
    /// ZIP archive.
    Zip,
}

/// Bounded source archive handoff from a transport.
pub struct ForgeArchive {
    source: Box<dyn Read>,
    /// Byte encoding of the reader's bytes.
    pub format: ForgeArchiveFormat,
    /// Optional top-level directory to strip before manifest admission.
    pub root_prefix: Option<Arc<str>>,
}

impl fmt::Debug for ForgeArchive {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeArchive")
            .field("format", &self.format)
            .field("root_prefix", &self.root_prefix)
            .field("streamed", &true)
            .finish()
    }
}

impl ForgeArchive {
    /// Wraps one owned archive reader. The reader is consumed exactly once by
    /// the acquisition service, which admits it directly into the shared CAS.
    pub fn from_reader<R: Read + 'static>(
        reader: R,
        format: ForgeArchiveFormat,
        root_prefix: Option<impl Into<String>>,
    ) -> Self {
        Self {
            source: Box::new(reader),
            format,
            root_prefix: root_prefix.map(|value| Arc::from(value.into())),
        }
    }

    /// Constructs a tar-gzip handoff with no implicit path stripping.
    #[must_use]
    pub fn tar_gzip(bytes: Vec<u8>) -> Self {
        Self::from_reader(
            Cursor::new(bytes),
            ForgeArchiveFormat::TarGzip,
            None::<String>,
        )
    }

    /// Constructs a plain tar handoff with an optional trusted root prefix.
    #[must_use]
    pub fn tar(bytes: Vec<u8>, root_prefix: Option<impl Into<String>>) -> Self {
        Self::from_reader(Cursor::new(bytes), ForgeArchiveFormat::Tar, root_prefix)
    }

    /// Constructs a zip handoff.
    #[must_use]
    pub fn zip(bytes: Vec<u8>) -> Self {
        Self::from_reader(Cursor::new(bytes), ForgeArchiveFormat::Zip, None::<String>)
    }

    pub(super) fn into_reader(self) -> Box<dyn Read> {
        self.source
    }
}

/// A bounded, transport-independent forge request.
pub trait ForgeTransport {
    /// Resolves a typed ref to an exact commit/tree object.
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError>;
    /// Fetches the exact source archive selected by `resolution`.
    fn fetch_archive(
        &mut self,
        _coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError>;
    /// Fetches bounded repository metadata. The default preserves typed
    /// unavailable facts and does not make a second network request.
    fn fetch_metadata(
        &mut self,
        coordinate: &ForgeCoordinate,
        _resolution: &ForgeResolution,
    ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
        Ok(ForgeRepositoryMetadata::unavailable(
            coordinate.owner(),
            ForgeUnavailableReason::Unsupported,
        ))
    }
}

/// Typed transport failures without endpoint or credential strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeTransportError {
    /// Source is unavailable or timed out.
    Unavailable,
    /// Source requested bounded retry.
    RetryAfter(u64),
    /// Remote object or repository was absent.
    NotFound,
    /// Response exceeded a configured bound.
    Bounds,
    /// Response did not satisfy the typed forge protocol.
    Protocol,
    /// Remote bytes did not match the exact selected object.
    Integrity,
    /// Credentials or local policy rejected the request.
    Policy,
}

mod git;
mod http;
pub use git::GitCommandTransport;
pub use http::{ForgeAuthToken, HttpForgeTransport};
