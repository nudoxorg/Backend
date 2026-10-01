//! Real declarations for the gallery and the tests: the shapes tokio's
//! `sync::mpsc` and toml's `de` actually spell, with their own first
//! sentences. Signatures are the source text an index records.

use super::state::Use;
use crate::icons::Kind;

/// One declaration: name, kind, signature, first sentence.
pub type Decl = (&'static str, Kind, &'static str, Option<&'static str>);

/// tokio::sync::mpsc.
pub const MPSC: &[Decl] = &[
    (
        "Sender",
        Kind::Struct,
        "pub struct Sender<T> {",
        Some("Sends values to the associated Receiver."),
    ),
    (
        "WeakSender",
        Kind::Struct,
        "pub struct WeakSender<T> {",
        Some("A sender that does not prevent the channel from being closed."),
    ),
    (
        "Permit",
        Kind::Struct,
        "pub struct Permit<'a, T> {",
        Some("Permits to send one value into the channel."),
    ),
    (
        "PermitIterator",
        Kind::Struct,
        "pub struct PermitIterator<'a, T> {",
        Some("An Iterator of Permit that can be used to hold n slots in the channel."),
    ),
    (
        "OwnedPermit",
        Kind::Struct,
        "pub struct OwnedPermit<T> {",
        Some("Owned permit to send one value into the channel."),
    ),
    (
        "Receiver",
        Kind::Struct,
        "pub struct Receiver<T> {",
        Some("Receives values from the associated Sender."),
    ),
    (
        "SendError",
        Kind::Struct,
        "pub struct SendError<T>(pub T);",
        Some("Error returned by Sender::send."),
    ),
    (
        "TrySendError",
        Kind::Enum,
        "pub enum TrySendError<T> {",
        Some("Error returned by Sender::try_send."),
    ),
    (
        "TryRecvError",
        Kind::Enum,
        "pub enum TryRecvError {",
        Some("Error returned by Receiver::try_recv."),
    ),
    ("RecvError", Kind::Struct, "pub struct RecvError;", None),
    (
        "UnboundedSender",
        Kind::Struct,
        "pub struct UnboundedSender<T> {",
        Some("Send values to the associated UnboundedReceiver."),
    ),
    (
        "channel",
        Kind::Function,
        "pub fn channel<T>(buffer: usize) -> (Sender<T>, Receiver<T>)",
        Some(
            "Creates a bounded mpsc channel for communicating between asynchronous tasks with backpressure.",
        ),
    ),
    (
        "unbounded_channel",
        Kind::Function,
        "pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>)",
        Some(
            "Creates an unbounded mpsc channel for communicating between asynchronous tasks without backpressure.",
        ),
    ),
    (
        "send",
        Kind::Method,
        "pub async fn send(&self, value: T) -> Result<(), SendError<T>>",
        Some("Sends a value, waiting until there is capacity."),
    ),
    (
        "try_recv",
        Kind::Method,
        "pub fn try_recv(&mut self) -> Result<T, TryRecvError>",
        Some("Tries to receive the next value for this receiver."),
    ),
    (
        "is_closed",
        Kind::Method,
        "pub fn is_closed(&self) -> bool",
        Some("Checks if the channel has been closed."),
    ),
];

/// toml::de and friends.
pub const TOML: &[Decl] = &[
    (
        "from_str",
        Kind::Function,
        "pub fn from_str<T>(s: &str) -> Result<T, Error> where T: DeserializeOwned",
        Some("Deserializes a string into a type."),
    ),
    (
        "to_string",
        Kind::Function,
        "pub fn to_string<T: ?Sized>(value: &T) -> Result<String, Error> where T: Serialize",
        Some("Serialize the given data structure as a String of TOML."),
    ),
    (
        "Deserializer",
        Kind::Struct,
        "pub struct Deserializer<'a> {",
        Some("Deserialization TOML document."),
    ),
    (
        "Value",
        Kind::Enum,
        "pub enum Value {",
        Some("Representation of a TOML value."),
    ),
    (
        "Table",
        Kind::Type,
        "pub type Table = Map<String, Value>;",
        Some("Type representing a TOML table, payload of the Value::Table variant."),
    ),
    (
        "Error",
        Kind::Struct,
        "pub struct Error {",
        Some("Errors that can occur when deserializing a type."),
    ),
    ("Datetime", Kind::Struct, "pub struct Datetime {", None),
    (
        "Index",
        Kind::Trait,
        "pub trait Index: Sealed {",
        Some("A type that can be used to index into a Value."),
    ),
    (
        "toml",
        Kind::Macro,
        "macro_rules! toml {",
        Some("Construct a Value from TOML syntax."),
    ),
    (
        "VERSION",
        Kind::Constant,
        "pub const VERSION: &'static str = \"0.8.23\";",
        None,
    ),
    (
        "from_utf8_unchecked",
        Kind::Function,
        "pub unsafe fn from_utf8_unchecked(v: &[u8]) -> &str",
        Some("Converts bytes without checking that they are valid UTF-8."),
    ),
];

/// A module's shingles, deterministic: mostly types, some callables, a
/// contract and a value now and then.
#[cfg(any(test, feature = "gallery"))]
#[must_use]
pub fn shingles(module: usize, count: usize) -> Vec<crate::folio::shingles::ShingleFacts> {
    use crate::tokens::Family;
    (0..count)
        .map(|j| crate::folio::shingles::ShingleFacts {
            name: format!("Item{module}_{j}").into(),
            family: match (module * 7 + j * 3) % 6 {
                0..=2 => Family::Type,
                3 | 4 => Family::Callable,
                _ if j % 2 == 0 => Family::Contract,
                _ => Family::Value,
            },
            yours: Use::Elsewhere,
            state: None,
        })
        .collect()
}

/// tokio's modules and how many names each holds.
pub const TOKIO_MODULES: &[(&str, usize)] = &[
    ("sync::mpsc", 15),
    ("net::unix", 14),
    ("doc::os", 12),
    ("signal::windows", 10),
    ("net::windows", 8),
    ("runtime::dump", 7),
    ("sync::broadcast", 7),
    ("sync::watch", 6),
    ("process", 5),
    ("sync::oneshot", 5),
    ("io::bsd", 3),
    ("signal::unix", 3),
    ("task::coop", 3),
    ("task::join_set", 2),
    ("time::error", 2),
    ("doc", 1),
];

/// toml's modules.
pub const TOML_MODULES: &[(&str, usize)] = &[
    ("lib", 1),
    ("map", 9),
    ("de", 4),
    ("macros", 4),
    ("ser", 4),
    ("value", 4),
];

/// tokio's releases (a slice of the real history), `(version, date)`.
#[cfg(any(test, feature = "gallery"))]
pub const TOKIO_RELEASES: &[(&str, &str)] = &[
    ("0.1.0", "2016-08-02"),
    ("0.1.1", "2016-08-04"),
    ("0.1.2", "2016-09-27"),
    ("0.1.3", "2016-10-10"),
    ("0.1.4", "2016-10-27"),
    ("0.1.5", "2017-01-04"),
    ("0.1.6", "2017-02-17"),
    ("0.1.7", "2017-04-06"),
    ("0.1.8", "2017-08-01"),
    ("0.1.9", "2017-11-23"),
    ("0.1.10", "2018-02-01"),
    ("0.1.11", "2018-07-26"),
    ("0.1.12", "2018-09-12"),
    ("0.1.13", "2018-11-14"),
    ("0.1.14", "2018-12-21"),
    ("0.1.16", "2019-02-01"),
    ("0.1.20", "2019-03-11"),
    ("0.1.22", "2019-06-26"),
    ("0.2.0", "2019-11-26"),
    ("0.2.1", "2019-12-03"),
    ("0.2.11", "2020-03-12"),
    ("0.2.22", "2020-06-11"),
    ("0.3.0", "2020-09-30"),
    ("0.3.5", "2020-11-19"),
    ("1.0.0", "2020-12-23"),
    ("1.0.2", "2021-01-13"),
    ("1.1.0", "2021-01-19"),
    ("1.2.0", "2021-01-26"),
    ("1.5.0", "2021-03-17"),
    ("1.8.0", "2021-05-15"),
    ("1.9.0", "2021-06-08"),
    ("1.12.0", "2021-08-03"),
    ("1.14.0", "2021-10-01"),
    ("1.17.0", "2022-02-05"),
    ("1.19.0", "2022-06-01"),
    ("1.21.0", "2022-09-01"),
    ("1.24.0", "2022-12-12"),
    ("1.25.0", "2023-01-05"),
    ("1.28.0", "2023-04-19"),
    ("1.29.0", "2023-06-01"),
    ("1.32.0", "2023-08-16"),
    ("1.35.0", "2023-12-19"),
    ("1.37.0", "2024-06-13"),
    ("1.38.0", "2024-07-04"),
    ("1.40.0", "2024-11-05"),
    ("1.42.0", "2025-01-08"),
    ("1.44.0", "2025-04-03"),
    ("1.45.0", "2025-07-01"),
    ("1.47.0", "2025-10-14"),
    ("1.50.0", "2026-03-11"),
    ("1.52.0", "2026-06-10"),
    ("1.52.3", "2026-07-30"),
    ("1.53.1", "2026-09-04"),
];

/// tokio's features and what each switches on, from its manifest:
/// `(feature, enables, optional packages it pulls in)`.
#[cfg(any(test, feature = "gallery"))]
pub const TOKIO_FEATURES: &[(&str, &[&str], &[&str])] = &[
    ("fs", &[], &[]),
    (
        "full",
        &[
            "fs",
            "io-util",
            "io-std",
            "macros",
            "net",
            "process",
            "rt",
            "rt-multi-thread",
            "signal",
            "sync",
            "time",
        ],
        &["parking_lot"],
    ),
    ("io-std", &[], &[]),
    ("io-uring", &[], &["io-uring", "libc", "mio", "slab"]),
    ("io-util", &[], &["bytes"]),
    ("macros", &[], &["tokio-macros"]),
    ("net", &[], &["libc", "mio", "socket2", "windows-sys"]),
    (
        "process",
        &[],
        &[
            "bytes",
            "libc",
            "mio",
            "signal-hook-registry",
            "windows-sys",
        ],
    ),
    ("rt", &[], &[]),
    ("rt-multi-thread", &["rt"], &[]),
    ("schedule-latency", &[], &[]),
    (
        "signal",
        &[],
        &["libc", "mio", "signal-hook-registry", "windows-sys"],
    ),
    ("sync", &[], &[]),
    ("taskdump", &[], &["backtrace"]),
    ("test-util", &["rt", "sync", "time"], &[]),
    ("time", &[], &[]),
    ("bytes", &[], &["bytes"]),
    ("mio", &[], &["mio"]),
    ("parking_lot", &[], &["parking_lot"]),
    ("tokio-macros", &[], &["tokio-macros"]),
    ("libc", &[], &["libc"]),
    ("socket2", &[], &["socket2"]),
    ("tracing", &[], &["tracing"]),
    ("signal-hook-registry", &[], &["signal-hook-registry"]),
    ("windows-sys", &[], &["windows-sys"]),
];

/// What each optional package of tokio weighs, in lines.
#[cfg(any(test, feature = "gallery"))]
pub const TOKIO_DEP_LINES: &[(&str, usize)] = &[
    ("bytes", 4059),
    ("mio", 8262),
    ("parking_lot", 3635),
    ("tokio-macros", 801),
    ("backtrace", 5584),
    ("io-uring", 14085),
    ("libc", 113_304),
    ("slab", 752),
    ("socket2", 5650),
    ("tracing", 3573),
    ("signal-hook-registry", 701),
    ("windows-sys", 334_264),
];

/// tokio's weight: `(name, lines, layer, parent, deps)`.
#[cfg(any(test, feature = "gallery"))]
pub const TOKIO_BERG: &[(&str, usize, usize, Option<usize>, &[usize])] = &[
    ("bytes", 4059, 0, None, &[]),
    ("mio", 8262, 0, None, &[8, 9]),
    ("parking_lot", 3635, 0, None, &[10, 11]),
    ("pin-project-lite", 1180, 0, None, &[]),
    ("tokio-macros", 801, 0, None, &[12, 13, 14]),
    ("socket2", 5650, 0, None, &[8, 9]),
    ("signal-hook-registry", 701, 0, None, &[8]),
    ("tracing", 3573, 0, None, &[15]),
    ("libc", 113_304, 1, Some(1), &[]),
    ("windows-sys", 334_264, 1, Some(1), &[16]),
    ("parking_lot_core", 2900, 1, Some(2), &[8, 17, 18]),
    ("lock_api", 2100, 1, Some(2), &[19]),
    ("proc-macro2", 6200, 1, Some(4), &[]),
    ("quote", 3100, 1, Some(4), &[12]),
    ("syn", 42_000, 1, Some(4), &[12, 13]),
    ("tracing-core", 3900, 1, Some(7), &[]),
    ("windows-link", 300, 2, Some(9), &[]),
    ("smallvec", 2600, 2, Some(10), &[]),
    ("cfg-if", 220, 2, Some(10), &[]),
    ("scopeguard", 340, 2, Some(11), &[]),
];
