macro_rules! protocol_registry {
    (
        domains { $(($marker:ident, $variant:ident, $code:literal, $label:literal)),+ $(,)? }
        encodings { $(($encoding:ident, $variant_encoding:ident, $encoding_code:literal, $encoding_label:literal)),+ $(,)? }
    ) => {
        #[repr(u8)]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        /// Closed protocol registry code for a content identity domain.
        pub enum DomainCode { $( #[doc = concat!("Registered domain code `", stringify!($code), ".")] $variant = $code ),+ }

        #[repr(u8)]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        /// Closed protocol registry code for an encoded artifact representation.
        pub enum EncodingCode { $( #[doc = concat!("Registered encoding code `", stringify!($encoding_code), ".")] $variant_encoding = $encoding_code ),+ }

        impl From<DomainCode> for u8 {
            #[allow(clippy::as_conversions, reason = "closed repr(u8) registry discriminant")]
            fn from(code: DomainCode) -> Self { code as u8 }
        }
        impl From<EncodingCode> for u8 {
            #[allow(clippy::as_conversions, reason = "closed repr(u8) registry discriminant")]
            fn from(code: EncodingCode) -> Self { code as u8 }
        }
        $(
            #[doc = concat!("Protocol-owned marker for `", stringify!($marker), "`.")]
            #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
            pub enum $marker {}
            impl sealed::Domain for $marker {}
            impl Domain for $marker {
                const TAG: DomainTag = DomainTag::new(*$label);
                const CODE: DomainCode = DomainCode::$variant;
            }
        )+
        $(
            #[doc = concat!("Protocol-owned marker for `", stringify!($encoding), "`.")]
            #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
            pub enum $encoding {}
            impl sealed::Encoding for $encoding {}
            impl Encoding for $encoding {
                const TAG: EncodingTag = EncodingTag::new(*$encoding_label);
                const CODE: EncodingCode = EncodingCode::$variant_encoding;
            }
        )+
    };
}
