//! Defines vocabulary behavior for `heart-schema`, whose purpose is to define shared binary schema limits, identifiers, and vocabulary.
//! This module owns the vocabulary invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{
    borrow::Borrow,
    fmt,
    mem::{align_of, offset_of, size_of},
    ops::Deref,
};

use crate::SchemaId;
use strum::{EnumCount, FromRepr, VariantArray};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32},
};

macro_rules! wire_byte {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(
            Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, Ord,
            PartialEq, PartialOrd, Unaligned,
        )]
        pub struct $name(u8);

        impl From<u8> for $name {
            fn from(value: u8) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u8 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Deref for $name {
            type Target = u8;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<u8> for $name {
            fn as_ref(&self) -> &u8 {
                self
            }
        }

        impl Borrow<u8> for $name {
            fn borrow(&self) -> &u8 {
                self
            }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        const _: [(); size_of::<$name>()] = [(); size_of::<u8>()];
        const _: [(); align_of::<$name>()] = [(); align_of::<u8>()];
    };
}

wire_byte!(
    /// Raw major-version byte in a borrowed frame header.
    ProtocolMajor
);
wire_byte!(
    /// Raw minor-version byte in a borrowed frame header.
    ProtocolMinor
);
wire_byte!(
    /// Raw discriminant byte in a borrowed section descriptor.
    SectionKindCode
);
wire_byte!(
    /// Raw compatibility-bit byte in a borrowed section descriptor.
    SectionFlags
);

/// Four-byte binary-frame envelope magic.
pub const FRAME_MAGIC: [u8; 4] = *b"NDX1";
/// Only supported envelope major version.
pub const FORMAT_MAJOR: ProtocolMajor = ProtocolMajor(1);
/// Only supported envelope minor version.
pub const FORMAT_MINOR: ProtocolMinor = ProtocolMinor(0);
/// Closed frame body schema tag.
pub const FRAME_SCHEMA_ID: SchemaId = SchemaId::Frame;
/// Header magic offset.
pub const HEADER_MAGIC_OFFSET: usize = offset_of!(FrameHeader, magic);
/// Header major-version offset.
pub const HEADER_MAJOR_OFFSET: usize = offset_of!(FrameHeader, major);
/// Header minor-version offset.
pub const HEADER_MINOR_OFFSET: usize = offset_of!(FrameHeader, minor);
/// Header length offset.
pub const HEADER_LENGTH_OFFSET: usize = offset_of!(FrameHeader, header_bytes);
/// Complete frame length offset.
pub const HEADER_TOTAL_LENGTH_OFFSET: usize = offset_of!(FrameHeader, total_bytes);
/// Directory count offset.
pub const HEADER_SECTION_COUNT_OFFSET: usize = offset_of!(FrameHeader, section_count);
/// First required-zero header field offset.
pub const HEADER_RESERVED_A_OFFSET: usize = offset_of!(FrameHeader, reserved_a);
/// Closed schema tag offset.
pub const HEADER_SCHEMA_OFFSET: usize = offset_of!(FrameHeader, schema);
/// Second required-zero header field offset.
pub const HEADER_RESERVED_B_OFFSET: usize = offset_of!(FrameHeader, reserved_b);
/// Fixed bytes in every frame header, derived from its field types.
pub const FRAME_HEADER_BYTES: usize = size_of::<FrameHeader>();
/// Raw section-kind offset within one descriptor.
pub const DESCRIPTOR_KIND_OFFSET: usize = offset_of!(SectionDescriptor, kind);
/// Raw descriptor-flags offset.
pub const DESCRIPTOR_FLAGS_OFFSET: usize = offset_of!(SectionDescriptor, flags);
/// Required-zero descriptor field offset.
pub const DESCRIPTOR_RESERVED_OFFSET: usize = offset_of!(SectionDescriptor, reserved);
/// Section body offset field.
pub const DESCRIPTOR_BODY_OFFSET: usize = offset_of!(SectionDescriptor, body_offset);
/// Section body length field.
pub const DESCRIPTOR_BODY_LENGTH_OFFSET: usize = offset_of!(SectionDescriptor, body_length);
/// Section row-count field.
pub const DESCRIPTOR_ROWS_OFFSET: usize = offset_of!(SectionDescriptor, rows);
/// Fixed descriptor width, derived from its field types.
pub const SECTION_DESCRIPTOR_BYTES: usize = size_of::<SectionDescriptor>();
/// Required alignment of every section body offset.
pub const SECTION_ALIGNMENT: usize = 8;
/// Sole defined flag: an unknown section may be skipped after its declared span validates.
pub const OPTIONAL_SECTION_FLAG: SectionFlags = SectionFlags(1);

/// Declarative fixed-width envelope record borrowed directly from canonical frame bytes.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq, Unaligned,
)]
pub struct FrameHeader {
    /// Four-byte protocol magic.
    pub magic: [u8; 4],
    /// Protocol major version.
    pub major: ProtocolMajor,
    /// Protocol minor version.
    pub minor: ProtocolMinor,
    /// Fixed header-plus-directory byte count.
    pub header_bytes: U16<LittleEndian>,
    /// Exact complete frame length.
    pub total_bytes: U32<LittleEndian>,
    /// Number of directory records.
    pub section_count: U16<LittleEndian>,
    /// Required-zero reserved header field.
    pub reserved_a: U16<LittleEndian>,
    /// Closed body schema identifier.
    pub schema: U32<LittleEndian>,
    /// Required-zero reserved header field.
    pub reserved_b: U32<LittleEndian>,
}

/// Declarative fixed-width directory record borrowed directly from canonical frame bytes.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq, Unaligned,
)]
pub struct SectionDescriptor {
    /// Raw closed-or-optional extension section code.
    pub kind: SectionKindCode,
    /// Compatibility flags for this section.
    pub flags: SectionFlags,
    /// Required-zero descriptor field.
    pub reserved: U16<LittleEndian>,
    /// Absolute aligned body offset.
    pub body_offset: U32<LittleEndian>,
    /// Exact section body byte length.
    pub body_length: U32<LittleEndian>,
    /// Bounded logical row count.
    pub rows: U32<LittleEndian>,
}

/// Known finite Wave 1 body kinds.
#[repr(u8)]
#[derive(
    Clone, Copy, Debug, EnumCount, Eq, FromRepr, Hash, Ord, PartialEq, PartialOrd, VariantArray,
)]
pub enum SectionKind {
    /// Small frame or batch metadata.
    Metadata = 1,
    /// Fixed or variable row records.
    Rows = 2,
    /// Opaque bytes referenced by rows.
    Data = 3,
}

impl From<SectionKind> for SectionKindCode {
    #[allow(
        clippy::as_conversions,
        reason = "SectionKind is repr(u8), and SectionKindCode is the explicit raw protocol boundary."
    )]
    fn from(kind: SectionKind) -> Self {
        Self(kind as u8)
    }
}

impl TryFrom<SectionKindCode> for SectionKind {
    type Error = UnknownSectionKind;

    fn try_from(value: SectionKindCode) -> Result<Self, Self::Error> {
        Self::from_repr(*value).ok_or(UnknownSectionKind(value))
    }
}

/// Section code not assigned by the Wave 1 registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownSectionKind(pub SectionKindCode);

/// Accepted compatibility disposition for one raw directory record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionDisposition {
    /// Known section supported by this decoder.
    Supported(SectionKind),
    /// Unknown section explicitly optional and safely skippable by validated length.
    SkippableOptional,
}

/// Exact rejected compatibility state for a raw directory record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SectionCompatibilityError {
    /// A defined known kind carried flags not assigned to known kinds.
    #[error("known section {kind:?} has unsupported flags {flags:#04x}")]
    KnownFlags {
        /// Decoded closed known kind.
        kind: SectionKind,
        /// Rejected raw flag byte.
        flags: SectionFlags,
    },
    /// An unnamed kind was required rather than explicitly optional.
    #[error("unknown required section kind {kind:#04x}")]
    UnknownRequired {
        /// Rejected unnamed raw kind code.
        kind: SectionKindCode,
    },
    /// An unnamed kind used flags outside the sole optional-extension assignment.
    #[error("unknown section {kind:#04x} has unsupported flags {flags:#04x}")]
    UnknownFlags {
        /// Rejected unnamed raw kind code.
        kind: SectionKindCode,
        /// Rejected raw flag byte.
        flags: SectionFlags,
    },
}

/// Classifies one raw descriptor without weakening rejected combinations into a catch-all value.
///
/// # Errors
///
/// Returns [`SectionCompatibilityError`] with the exact rejected kind and flags when the record
/// does not obey the finite Wave 1 compatibility law.
pub fn section_compatibility(
    kind: SectionKindCode,
    flags: SectionFlags,
) -> Result<SectionDisposition, SectionCompatibilityError> {
    match SectionKind::try_from(kind) {
        Ok(known) if *flags == 0 => Ok(SectionDisposition::Supported(known)),
        Ok(known) => Err(SectionCompatibilityError::KnownFlags { kind: known, flags }),
        Err(_) if flags == OPTIONAL_SECTION_FLAG => Ok(SectionDisposition::SkippableOptional),
        Err(unknown) if *flags == 0 => {
            Err(SectionCompatibilityError::UnknownRequired { kind: unknown.0 })
        }
        Err(unknown) => Err(SectionCompatibilityError::UnknownFlags {
            kind: unknown.0,
            flags,
        }),
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{offset_of, size_of};

    use strum::{EnumCount, VariantArray};
    use zerocopy::{FromBytes, Unaligned};

    use super::{
        DESCRIPTOR_BODY_LENGTH_OFFSET, DESCRIPTOR_BODY_OFFSET, DESCRIPTOR_FLAGS_OFFSET,
        DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_RESERVED_OFFSET, DESCRIPTOR_ROWS_OFFSET,
        FRAME_HEADER_BYTES, FrameHeader, HEADER_LENGTH_OFFSET, HEADER_MAGIC_OFFSET,
        HEADER_MAJOR_OFFSET, HEADER_MINOR_OFFSET, HEADER_RESERVED_A_OFFSET,
        HEADER_RESERVED_B_OFFSET, HEADER_SCHEMA_OFFSET, HEADER_SECTION_COUNT_OFFSET,
        HEADER_TOTAL_LENGTH_OFFSET, OPTIONAL_SECTION_FLAG, ProtocolMajor, ProtocolMinor,
        SECTION_DESCRIPTOR_BYTES, SectionCompatibilityError, SectionDescriptor, SectionDisposition,
        SectionFlags, SectionKind, SectionKindCode, UnknownSectionKind, section_compatibility,
    };

    #[derive(Clone, Copy)]
    struct WireRecordLayout {
        expected: usize,
        exported: usize,
        declared: usize,
    }

    #[derive(Clone, Copy)]
    struct WireFieldLayout {
        expected: usize,
        exported: usize,
        declared: usize,
    }

    const FRAME_HEADER_LAYOUT: WireRecordLayout = WireRecordLayout {
        expected: 24,
        exported: FRAME_HEADER_BYTES,
        declared: size_of::<FrameHeader>(),
    };

    const FRAME_HEADER_FIELDS: [WireFieldLayout; 9] = [
        WireFieldLayout {
            expected: 0,
            exported: HEADER_MAGIC_OFFSET,
            declared: offset_of!(FrameHeader, magic),
        },
        WireFieldLayout {
            expected: 4,
            exported: HEADER_MAJOR_OFFSET,
            declared: offset_of!(FrameHeader, major),
        },
        WireFieldLayout {
            expected: 5,
            exported: HEADER_MINOR_OFFSET,
            declared: offset_of!(FrameHeader, minor),
        },
        WireFieldLayout {
            expected: 6,
            exported: HEADER_LENGTH_OFFSET,
            declared: offset_of!(FrameHeader, header_bytes),
        },
        WireFieldLayout {
            expected: 8,
            exported: HEADER_TOTAL_LENGTH_OFFSET,
            declared: offset_of!(FrameHeader, total_bytes),
        },
        WireFieldLayout {
            expected: 12,
            exported: HEADER_SECTION_COUNT_OFFSET,
            declared: offset_of!(FrameHeader, section_count),
        },
        WireFieldLayout {
            expected: 14,
            exported: HEADER_RESERVED_A_OFFSET,
            declared: offset_of!(FrameHeader, reserved_a),
        },
        WireFieldLayout {
            expected: 16,
            exported: HEADER_SCHEMA_OFFSET,
            declared: offset_of!(FrameHeader, schema),
        },
        WireFieldLayout {
            expected: 20,
            exported: HEADER_RESERVED_B_OFFSET,
            declared: offset_of!(FrameHeader, reserved_b),
        },
    ];

    const SECTION_DESCRIPTOR_LAYOUT: WireRecordLayout = WireRecordLayout {
        expected: 16,
        exported: SECTION_DESCRIPTOR_BYTES,
        declared: size_of::<SectionDescriptor>(),
    };

    const SECTION_DESCRIPTOR_FIELDS: [WireFieldLayout; 6] = [
        WireFieldLayout {
            expected: 0,
            exported: DESCRIPTOR_KIND_OFFSET,
            declared: offset_of!(SectionDescriptor, kind),
        },
        WireFieldLayout {
            expected: 1,
            exported: DESCRIPTOR_FLAGS_OFFSET,
            declared: offset_of!(SectionDescriptor, flags),
        },
        WireFieldLayout {
            expected: 2,
            exported: DESCRIPTOR_RESERVED_OFFSET,
            declared: offset_of!(SectionDescriptor, reserved),
        },
        WireFieldLayout {
            expected: 4,
            exported: DESCRIPTOR_BODY_OFFSET,
            declared: offset_of!(SectionDescriptor, body_offset),
        },
        WireFieldLayout {
            expected: 8,
            exported: DESCRIPTOR_BODY_LENGTH_OFFSET,
            declared: offset_of!(SectionDescriptor, body_length),
        },
        WireFieldLayout {
            expected: 12,
            exported: DESCRIPTOR_ROWS_OFFSET,
            declared: offset_of!(SectionDescriptor, rows),
        },
    ];

    fn assert_wire_record_layout(layout: WireRecordLayout, fields: &[WireFieldLayout]) {
        assert_eq!(layout.declared, layout.exported);
        assert_eq!(layout.exported, layout.expected);
        for field in fields {
            assert_eq!(field.declared, field.exported);
            assert_eq!(field.exported, field.expected);
        }
    }

    #[test]
    fn optional_extensions_are_the_only_open_compatibility_path() {
        assert_eq!(
            section_compatibility(
                SectionKindCode::from(SectionKind::Rows),
                SectionFlags::from(0)
            ),
            Ok(SectionDisposition::Supported(SectionKind::Rows))
        );
        assert_eq!(
            section_compatibility(SectionKindCode::from(0x40), SectionFlags::from(1)),
            Ok(SectionDisposition::SkippableOptional)
        );
        assert_eq!(
            section_compatibility(SectionKindCode::from(0x40), SectionFlags::from(0)),
            Err(SectionCompatibilityError::UnknownRequired {
                kind: SectionKindCode::from(0x40),
            })
        );
        assert_eq!(
            section_compatibility(
                SectionKindCode::from(SectionKind::Metadata),
                SectionFlags::from(1),
            ),
            Err(SectionCompatibilityError::KnownFlags {
                kind: SectionKind::Metadata,
                flags: SectionFlags::from(1),
            })
        );
    }

    #[test]
    fn known_section_vocabulary_is_derived_and_exhaustively_decodable() {
        let variants = <SectionKind as VariantArray>::VARIANTS;
        assert_eq!(variants.len(), <SectionKind as EnumCount>::COUNT);
        assert_eq!(variants.len(), 3);
        for kind in variants {
            assert_eq!(
                SectionKind::try_from(SectionKindCode::from(*kind)),
                Ok(*kind)
            );
        }
    }

    #[test]
    fn wire_byte_scalars_preserve_raw_cells_without_claiming_vocabulary_validity() {
        assert_eq!(u8::from(ProtocolMajor::from(0xfe)), 0xfe);
        assert_eq!(u8::from(ProtocolMinor::from(0x7f)), 0x7f);
        assert_eq!(
            SectionKindCode::from(SectionKind::Rows),
            SectionKindCode::from(2)
        );
        assert_eq!(
            SectionKind::try_from(SectionKindCode::from(0xff)),
            Err(UnknownSectionKind(SectionKindCode::from(0xff)))
        );
        assert_eq!(
            section_compatibility(SectionKindCode::from(0x80), OPTIONAL_SECTION_FLAG),
            Ok(SectionDisposition::SkippableOptional)
        );
    }

    #[test]
    fn every_raw_kind_and_flag_pair_obeys_the_compatibility_law() {
        for kind in u8::MIN..=u8::MAX {
            for flags in u8::MIN..=u8::MAX {
                let expected = match SectionKind::from_repr(kind) {
                    Some(known) if flags == 0 => Ok(SectionDisposition::Supported(known)),
                    Some(known) => Err(SectionCompatibilityError::KnownFlags {
                        kind: known,
                        flags: SectionFlags::from(flags),
                    }),
                    None if flags == *OPTIONAL_SECTION_FLAG => {
                        Ok(SectionDisposition::SkippableOptional)
                    }
                    None if flags == 0 => Err(SectionCompatibilityError::UnknownRequired {
                        kind: SectionKindCode::from(kind),
                    }),
                    None => Err(SectionCompatibilityError::UnknownFlags {
                        kind: SectionKindCode::from(kind),
                        flags: SectionFlags::from(flags),
                    }),
                };
                assert_eq!(
                    section_compatibility(SectionKindCode::from(kind), SectionFlags::from(flags)),
                    expected
                );
            }
        }
    }

    #[test]
    fn wire_layout_widths_are_derived_from_the_declared_field_types() {
        assert_wire_record_layout(FRAME_HEADER_LAYOUT, &FRAME_HEADER_FIELDS);
        assert_wire_record_layout(SECTION_DESCRIPTOR_LAYOUT, &SECTION_DESCRIPTOR_FIELDS);
    }

    #[test]
    fn wire_records_are_total_and_unaligned_after_an_exact_width_proof() {
        fn assert_total_unaligned<Record: FromBytes + Unaligned>() {}

        assert_total_unaligned::<FrameHeader>();
        assert_total_unaligned::<SectionDescriptor>();
    }
}
