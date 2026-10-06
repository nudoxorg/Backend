//! Shared closed wire generation of canonical compiler package manifests.

/// Fixed header width of a compiler package manifest.
pub const COMPILATION_MANIFEST_HEADER_BYTES: usize = 16;
/// Fixed width of one complete compact fragment entry.
pub const COMPILATION_MANIFEST_ENTRY_BYTES: usize = 404;
/// Fixed width of one compact fragment paired with its complete semantic image.
pub const COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES: usize = 440;
/// Exact canonical compiler package manifest magic.
pub const COMPILATION_MANIFEST_MAGIC: [u8; 8] = *b"NUDXCPM\0";

/// Closed compiler-manifest generations shared by producers and durable readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilationManifestFormat {
    /// Compatibility package binding only compact IR fragments.
    CompactV1,
    /// Authoritative package binding compact fragments and full semantic images.
    SemanticV2,
    /// Contextual package ordered by compact and full-image identity.
    SemanticV3,
}

impl CompilationManifestFormat {
    /// Whether this format binds a complete semantic image for each compact entry.
    #[must_use]
    pub const fn is_semantic(self) -> bool {
        matches!(self, Self::SemanticV2 | Self::SemanticV3)
    }

    /// Admits only an explicitly supported complete wire generation.
    #[must_use]
    pub const fn from_version(version: u16) -> Option<Self> {
        match version {
            1 => Some(Self::CompactV1),
            2 => Some(Self::SemanticV2),
            3 => Some(Self::SemanticV3),
            _ => None,
        }
    }

    /// Exact wire generation; unrelated metadata schemas retain their own version.
    #[must_use]
    pub const fn wire_version(self) -> u16 {
        match self {
            Self::CompactV1 => 1,
            Self::SemanticV2 => 2,
            Self::SemanticV3 => 3,
        }
    }

    /// Exact fixed entry width admitted by this generation.
    #[must_use]
    pub const fn entry_bytes(self) -> usize {
        match self {
            Self::CompactV1 => COMPILATION_MANIFEST_ENTRY_BYTES,
            Self::SemanticV2 | Self::SemanticV3 => COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
        }
    }
}
