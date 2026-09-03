//! Defines typed authority, parse, capacity, and source failures for the collector.
//! Each failure preserves its deciding native status or exact required caller capacity.
//! No error path silently turns missing libclang facts into scanner-derived facts.

use thiserror::Error;

/// One caller-provided fact region accepted by [`crate::ClangScratch`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ScratchLane {
    /// Declaration fact slots.
    #[error("declarations")]
    Declarations,
    /// Recursive type fact slots.
    #[error("types")]
    Types,
    /// Recursive type edge slots.
    #[error("type edges")]
    TypeEdges,
    /// Reference fact slots.
    #[error("references")]
    References,
    /// Diagnostic fact slots.
    #[error("diagnostics")]
    Diagnostics,
    /// Include authority fact slots.
    #[error("includes")]
    Includes,
    /// C++ override-authority fact slots.
    #[error("overrides")]
    Overrides,
}

/// One exact libclang API required by this direct authority collector.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NativeApi {
    /// Translation-unit construction.
    #[error("translation-unit construction")]
    TranslationUnit,
    /// Cursor traversal.
    #[error("cursor traversal")]
    Traversal,
    /// Cursor locations and extents.
    #[error("source locations")]
    Locations,
    /// Cursor identities and references.
    #[error("cursor identities")]
    Identities,
    /// Type inspection and recursive type children.
    #[error("type inspection")]
    Types,
    /// Documentation comment ranges.
    #[error("documentation comments")]
    Documentation,
    /// Diagnostics.
    #[error("diagnostics")]
    Diagnostics,
    /// Include-directive authority.
    #[error("include directives")]
    Includes,
}

/// A native library failure with the original loader text retained losslessly.
#[derive(Debug, Error)]
#[error("libclang could not load: {detail}")]
pub struct NativeFailure {
    /// The exact loader-provided cause, owned only on this cold error path.
    pub detail: Box<str>,
}

/// One libclang translation-unit creation status.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ParseFailure {
    /// libclang reported an unspecified parse failure.
    #[error("native failure")]
    Failure,
    /// libclang crashed while producing the translation unit.
    #[error("native crash")]
    Crashed,
    /// The typed request violated libclang's argument contract.
    #[error("invalid arguments")]
    InvalidArguments,
    /// libclang could not read the requested abstract syntax tree.
    #[error("AST read failure")]
    AstRead,
    /// A newer libclang status code was observed without a matching enum variant.
    #[error("unknown native parse status {raw}")]
    Unknown {
        /// Exact raw status from libclang.
        raw: i32,
    },
}

/// The closed failure vocabulary of one fact collection attempt.
#[derive(Debug, Error)]
pub enum CollectError {
    /// A caller cancelled before native loading or at the next native cursor boundary.
    #[error("collection cancelled")]
    Cancelled,
    /// The source contains an interior NUL byte, which libclang cannot accept as this input.
    #[error("source contains an interior NUL byte")]
    SourceContainsNul,
    /// Source length exceeds libclang's unsigned-long length boundary.
    #[error("source length {observed} exceeds the native unsigned-long boundary")]
    SourceTooLarge {
        /// Exact rejected source length.
        observed: usize,
    },
    /// Source length cannot be represented by the collector's canonical byte coordinates.
    #[error("source length {observed} exceeds canonical byte-coordinate capacity")]
    SourceLengthTooLarge {
        /// Exact rejected caller source length.
        observed: usize,
    },
    /// The dynamic library loader rejected the available libclang authority.
    #[error(transparent)]
    Library(#[from] NativeFailure),
    /// The loaded library lacks an API necessary for complete fact collection.
    #[error("loaded libclang lacks {api}")]
    MissingApi {
        /// Exact unavailable API family.
        api: NativeApi,
    },
    /// libclang could not create its index object.
    #[error("libclang returned no index")]
    IndexUnavailable,
    /// libclang parsed the source but did not bind the declared main source file.
    #[error("libclang did not bind the declared main source file")]
    MainFileUnavailable,
    /// libclang rejected translation-unit creation.
    #[error("libclang translation-unit creation failed: {failure}")]
    Parse {
        /// Exact mapped native status.
        failure: ParseFailure,
    },
    /// A fact region lacks the exact next slot required to retain a native fact.
    #[error("{lane} capacity {capacity} cannot retain required slot {required}")]
    ScratchCapacity {
        /// The rejected caller storage region.
        lane: ScratchLane,
        /// Exact caller-provided number of slots.
        capacity: usize,
        /// Exact number of slots needed including the rejected fact.
        required: usize,
    },
    /// Native source coordinates cannot be represented by the canonical u32 span type.
    #[error("native source coordinate {coordinate} exceeds u32")]
    CoordinateTooLarge {
        /// Exact rejected coordinate.
        coordinate: u64,
    },
    /// A caller slot ordinal cannot be represented by the matching typed u32 identity.
    #[error("{lane} slot ordinal {observed} exceeds u32")]
    SlotOrdinalTooLarge {
        /// The typed caller storage lane.
        lane: ScratchLane,
        /// Exact rejected caller slot ordinal.
        observed: usize,
    },
    /// A caller slot counter cannot advance because it already equals `usize::MAX`.
    #[error("{lane} slot count overflowed at capacity {capacity}")]
    SlotCountOverflow {
        /// The typed caller storage lane.
        lane: ScratchLane,
        /// Exact caller capacity at the overflow boundary.
        capacity: usize,
    },
}
