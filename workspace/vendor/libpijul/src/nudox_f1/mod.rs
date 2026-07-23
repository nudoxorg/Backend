//! `nudox_f1` — frozen F1 key registry and field-aware line pairing.
//!
//! Inlined from the former standalone `nudox-f1` leaf crate (DEPTH1 plan §3).
//! Kept as a sub-module of `libpijul` so the cycle
//! `nudox-ir → libpijul → nudox-f1 → (cycle)` is broken: `nudox-ir` now
//! reaches the registry constants and the pairer via `libpijul::nudox_f1::…`.
//!
//! # Contents
//!
//! * [`registry`] — the frozen version-1 key table ([`REGISTRY`],
//!   [`Class`], [`KeySpec`], [`MAGIC_LINE`], [`MAGIC_STR`], [`key_of`],
//!   [`class_of`]).
//! * [`pair`] — the pairing function ([`line_diff`], [`LineReplace`]).

pub mod pair;
pub mod registry;

// ---------------------------------------------------------------------------
// Top-level re-exports (mirrors the former crate's public surface)
// ---------------------------------------------------------------------------

pub use pair::{line_diff, LineReplace};
pub use registry::{
    class_of,
    key_of,
    Class,
    KeySpec,
    MAGIC_LINE,
    MAGIC_STR,
    REGISTRY,
    KEY_ALIAS,
    KEY_ATTR,
    KEY_AUTO,
    KEY_CFG,
    KEY_CTY,
    KEY_CVAL,
    KEY_DEPRECATED,
    KEY_DLINK,
    KEY_DOC,
    KEY_FIELDTY,
    KEY_FNSIG,
    KEY_GPARAM,
    KEY_IFLAGS,
    KEY_IN,
    KEY_IOF,
    KEY_IFOR,
    KEY_KIND,
    KEY_LFACT,
    KEY_LINK,
    KEY_MAGIC,
    KEY_NAME,
    KEY_OUT,
    KEY_PARENT,
    KEY_RECFIELD,
    KEY_RECFORM,
    KEY_RETGT,
    KEY_SPAN,
    KEY_SRC,
    KEY_SUPER,
    KEY_TFLAGS,
    KEY_TYPE,
    KEY_VDISCR,
    KEY_VIS,
    KEY_VFORM,
    KEY_WHERE,
};
