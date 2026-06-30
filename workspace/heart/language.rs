//! The core language enumeration, shared across the compiler, registry,
//! runtime, and server.

use std::marker::ConstParamTy;

use semver::Version;

macro_rules! define_language_enums {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident($payload:ty)
        ),* $(,)?
    ) => {
        pub enum Language {
            $(
                $(#[$meta])*
                $variant($payload),
            )*
        }

        /// A payload-free language tag
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ConstParamTy)]
        pub enum LanguageTag {
            $(
                $variant,
            )*
        }

        impl Language {
            /// Extracts the payload-free tag from the language enum.
            pub const fn tag(&self) -> LanguageTag {
                match self {
                    $(
                        Self::$variant(_) => LanguageTag::$variant,
                    )*
                }
            }
        }
    };
}

/// A particular language variant, and its identifying information for the
/// toolchain it was built on (so like rust 1.89 + edition 2024) Some languages
/// will only need a version.

define_language_enums! {
		/// https://rust-lang.org/
		Rust(Version),

		/// https://www.typescriptlang.org/
		Typescript(Version),
}
