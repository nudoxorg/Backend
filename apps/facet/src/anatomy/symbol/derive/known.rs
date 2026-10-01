//! What the languages themselves name.
//!
//! The standard traits (`Clone`, `Deserialize`, `Read`, ...) mean the same
//! thing in every crate, so the page says what they mean in words once, here,
//! keyed by an enum. Nothing else in the derivation compares a trait name to
//! a string: `rail` asks [`Std::chip`], the call's generics ask
//! [`Std::means`], [`Std::parameter`] and [`Std::choice`], and the "If it
//! fails" section asks [`Source::possible`].
//!
//! Type vocabulary (`Vec`, `Option`, `str`) lives in `words`, and the rule
//! for what a name reads as when it wraps something (`Task<T>` yes, a plain
//! `Task` no) is there too.

use super::super::view::{Call, CapMark, Generic, Lang};
use super::text::last_segment;

/// A trait the language or its most common library defines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Std {
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Debug,
    Display,
    Default,
    Serialize,
    Deserialize,
    Serializer,
    Deserializer,
    IntoDeserializer,
    FromStr,
    ToString,
    Index,
    Error,
    Iterator,
    IntoIterator,
    Read,
    Write,
    Send,
    Sync,
    Unpin,
    UnwindSafe,
    AsRef,
    Into,
    From,
    Future,
    IntoFuture,
    Fn,
}

impl Std {
    /// The trait a name (or path) says, by its last segment.
    pub(super) fn of(name: &str) -> Option<Self> {
        let head = name.trim().split(['<', '(']).next().unwrap_or(name);
        let name = last_segment(head);
        Some(match name {
            "Clone" => Self::Clone,
            "Copy" => Self::Copy,
            "PartialEq" => Self::PartialEq,
            "Eq" => Self::Eq,
            "PartialOrd" => Self::PartialOrd,
            "Ord" => Self::Ord,
            "Hash" => Self::Hash,
            "Debug" => Self::Debug,
            "Display" => Self::Display,
            "Default" => Self::Default,
            "Serialize" => Self::Serialize,
            "Deserialize" | "DeserializeOwned" => Self::Deserialize,
            "Serializer" => Self::Serializer,
            "Deserializer" => Self::Deserializer,
            "IntoDeserializer" => Self::IntoDeserializer,
            "FromStr" => Self::FromStr,
            "ToString" => Self::ToString,
            "Index" => Self::Index,
            "Error" | "StdError" => Self::Error,
            "Iterator" => Self::Iterator,
            "IntoIterator" => Self::IntoIterator,
            "Read" | "BufRead" | "AsyncRead" => Self::Read,
            "Write" | "AsyncWrite" => Self::Write,
            "Send" => Self::Send,
            "Sync" => Self::Sync,
            "Unpin" | "RefUnwindSafe" => Self::Unpin,
            "UnwindSafe" => Self::UnwindSafe,
            "AsRef" => Self::AsRef,
            "Into" => Self::Into,
            "From" => Self::From,
            "Future" => Self::Future,
            "IntoFuture" => Self::IntoFuture,
            "Fn" | "FnMut" | "FnOnce" => Self::Fn,
            _ => return None,
        })
    }

    /// The chip a type that implements it gets.
    pub(super) const fn chip(self) -> Chip {
        match self {
            Self::Clone => Chip::Says(CapMark::Copy, "copies"),
            Self::Copy => Chip::Says(CapMark::Copy, "copies by assignment"),
            Self::PartialEq => Chip::Says(CapMark::Eq, "compares with =="),
            Self::PartialOrd | Self::Ord => Chip::Says(CapMark::Order, "orders"),
            Self::Hash => Chip::Says(CapMark::Hash, "can be a map key"),
            Self::Debug => Chip::Says(CapMark::Debug, "prints for debugging"),
            Self::Display => Chip::Says(CapMark::Print, "prints"),
            Self::Default => Chip::Says(CapMark::Default, "has a default"),
            Self::Serialize => Chip::Says(CapMark::Ser, "serde can write it"),
            Self::Deserialize => Chip::Says(CapMark::De, "serde can read one"),
            Self::FromStr => Chip::Says(CapMark::FromStr, "parses from text"),
            Self::Index => Chip::Says(CapMark::Index, "v[i] works"),
            Self::Deserializer => Chip::Says(CapMark::Reader, "is itself a reader of serde data"),
            Self::Error => Chip::Says(CapMark::Error, "is an error"),
            Self::Iterator | Self::IntoIterator => Chip::Says(CapMark::Iter, "loops"),
            // Implied by another chip, or true of nearly everything.
            Self::Eq | Self::IntoDeserializer | Self::Send | Self::Sync | Self::Unpin | Self::UnwindSafe => Chip::Hidden,
            Self::Serializer | Self::ToString | Self::Read | Self::Write | Self::AsRef | Self::Into | Self::From | Self::Future | Self::IntoFuture | Self::Fn => Chip::Named,
        }
    }

    /// What a bound on it means, for a generic's card.
    pub(super) const fn means(self) -> &'static str {
        match self {
            Self::Deserialize => "can be read by serde (any format)",
            Self::Serialize => "can be written by serde (any format)",
            Self::Serializer => "a serializer: a format's writer",
            Self::Deserializer => "a deserializer: a format's reader",
            Self::IntoDeserializer => "can turn itself into a deserializer",
            Self::Index => "an index: a position or a key",
            Self::Ord | Self::PartialOrd => "can be ordered",
            Self::Hash => "can be hashed",
            Self::Clone => "can be copied",
            Self::Copy => "is copied by assignment",
            Self::Debug => "prints for debugging",
            Self::Display => "prints",
            Self::Send => "can move between threads",
            Self::Sync => "can be shared between threads",
            Self::IntoIterator => "can be looped over",
            Self::Iterator => "gives one at a time",
            Self::AsRef => "can be read as",
            Self::Into => "turns into",
            Self::From => "made from",
            Self::Read => "a reader (io::Read)",
            Self::Write => "a writer (io::Write)",
            Self::Future => "a future",
            Self::IntoFuture => "anything you can await",
            Self::Fn => "a function",
            Self::Default => "has a default",
            Self::PartialEq | Self::Eq => "can be compared",
            Self::Error => "is an error",
            Self::FromStr => "parses from text",
            Self::ToString => "prints to text",
            Self::Unpin => "can be moved after it is pinned",
            Self::UnwindSafe => "survives a panic",
        }
    }

    /// The word for a parameter typed only by this bound.
    pub(super) const fn parameter(self) -> Option<Parameter> {
        Some(match self {
            Self::Serializer => Parameter::Word("a serializer"),
            Self::Deserializer => Parameter::Word("a deserializer"),
            Self::Index => Parameter::Word("an index"),
            Self::Read => Parameter::Word("a reader"),
            Self::Write => Parameter::Word("a writer"),
            Self::Fn => Parameter::Word("a function"),
            Self::IntoIterator | Self::Iterator => Parameter::Word("anything to loop over"),
            Self::Into | Self::AsRef | Self::From => Parameter::Converts,
            _ => return None,
        })
    }

    /// What "you choose it" says when a generic that only shows in the
    /// output is bounded by this trait.
    pub(super) const fn choice(self) -> Option<&'static str> {
        match self {
            Self::Deserialize => Some("You choose it: whatever you read the input into."),
            Self::Default => Some("You choose it: whatever you want made."),
            Self::FromStr => Some("You choose it: whatever you parse the text into."),
            _ => None,
        }
    }

    /// Whether a value bounded by it reaches outside the program: a reader, a writer.
    pub(super) const fn reaches_out(self) -> bool {
        matches!(self, Self::Read | Self::Write)
    }
}

/// What a trait a type implements becomes on the rail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Chip {
    /// Nothing: it is plumbing, or another chip says it.
    Hidden,
    /// A mark and words.
    Says(CapMark, &'static str),
    /// Its own name.
    Named,
}

/// How a bound names a parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Parameter {
    /// A fixed word.
    Word(&'static str),
    /// Whatever it converts from or to: the word comes from the bound's argument.
    Converts,
}

/// A method the language gives a special way to call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Conv {
    /// `From::from`: many impls, one row that lists what it converts from.
    From,
    /// `FromStr::from_str`, called as `text.parse()`.
    FromStr,
}

impl Conv {
    /// The conversion a method of a Rust type is, by its name.
    pub(super) fn of(lang: Lang, name: &str) -> Option<Self> {
        match (lang, name) {
            (Lang::Rust, "from") => Some(Self::From),
            (Lang::Rust, "from_str") => Some(Self::FromStr),
            _ => None,
        }
    }

    /// The name you call it by.
    pub(super) const fn called(self) -> Option<&'static str> {
        match self {
            Self::From => None,
            Self::FromStr => Some("parse"),
        }
    }

    /// The row's one line.
    pub(super) fn doc(self, owner: &str) -> String {
        match self {
            Self::From => format!("{owner}::from(x) or x.into(), from any of these"),
            Self::FromStr => "read one from text (str::parse)".to_owned(),
        }
    }
}

/// A failure kind named after where the failure comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Source {
    /// The outside world: a file, a socket, a stream.
    Io,
}

impl Source {
    /// The source a failure kind is named after.
    pub(super) fn of(kind: &str) -> Option<Self> {
        match kind {
            "Io" | "IO" | "IoError" | "Os" => Some(Self::Io),
            _ => None,
        }
    }

    /// Whether the call can reach that source: it takes a reader, a writer, a
    /// path or a file, as a bound or as a plain word.
    pub(super) fn possible(self, call: &Call, generics: &[Generic]) -> bool {
        match self {
            Self::Io => call.ports.iter().any(|port| {
                let bounded = port
                    .ty
                    .generic
                    .as_deref()
                    .and_then(|name| generics.iter().find(|generic| generic.name == name))
                    .is_some_and(|generic| generic.bounds.iter().filter_map(|bound| Std::of(&bound.name)).any(Std::reaches_out));
                let word = port.ty.word.to_ascii_lowercase();
                bounded || ["reader", "writer", "stream", "file", "path"].iter().any(|w| word.contains(w))
            }),
        }
    }

    /// Why a call that cannot reach it never fails that way.
    pub(super) fn why_not(self, kind: &str) -> String {
        match self {
            Self::Io => format!("{kind} can't happen here: nothing it takes reads from outside the program."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traits_are_read_by_their_last_segment() {
        assert_eq!(Std::of("serde::de::DeserializeOwned"), Some(Std::Deserialize));
        assert_eq!(Std::of("io::Read"), Some(Std::Read));
        assert_eq!(Std::of("FnMut(&str) -> bool"), Some(Std::Fn));
        assert_eq!(Std::of("Mystery"), None);
    }

    #[test]
    fn plumbing_traits_have_no_chip_and_the_rest_do() {
        assert_eq!(Std::Send.chip(), Chip::Hidden);
        assert_eq!(Std::IntoDeserializer.chip(), Chip::Hidden);
        assert_eq!(Std::Read.chip(), Chip::Named);
        assert_eq!(Std::Clone.chip(), Chip::Says(CapMark::Copy, "copies"));
        assert_eq!(Std::of("StdError"), Std::of("Error"));
    }

    #[test]
    fn a_reader_bound_makes_io_possible() {
        use super::super::super::view::{Bound, Joint, Origin, Port, Role, Ty};
        let mut call = Call::default();
        let mut ty = Ty::plain("R");
        ty.generic = Some("R".to_owned());
        call.ports.push(Port { name: "rdr".to_owned(), joint: Joint::Required, ty, default: None, note: None, options: Vec::new() });
        let none: Vec<Generic> = Vec::new();
        assert!(!Source::Io.possible(&call, &none), "a bare generic reaches nowhere");
        let reader = Generic { name: "R".to_owned(), role: Role::Needs, bounds: vec![Bound { name: "Read".to_owned(), means: String::new() }], says: String::new(), origin: Origin::Declared };
        assert!(Source::Io.possible(&call, &[reader]));
    }
}
