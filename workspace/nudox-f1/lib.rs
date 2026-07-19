//! `nudox-f1` — frozen F1 key registry and field-aware line pairing.
//!
//! This is the leaf crate that both the IR plane (`nudox-ir-vcs`) and the
//! libpijul fork must agree on.  It is pure data + pure functions over byte
//! slices: no libpijul types, no graph awareness, no I/O.  The only dependency
//! is `diffs`, pinned to the same version both consumers resolve, so the Myers
//! implementation is identical on both sides.
//!
//! # Contents
//!
//! * [`registry`] — the frozen version-1 key table ([`REGISTRY`],
//!   [`Class`], [`KeySpec`], [`MAGIC_LINE`], [`MAGIC_STR`], [`key_of`],
//!   [`class_of`]).
//! * [`pair`] — the pairing function ([`line_diff`], [`LineReplace`]).
//!
//! # Usage
//!
//! ```rust,ignore
//! use nudox_f1::{line_diff, MAGIC_LINE};
//!
//! // The fork sniff:
//! fn is_f1(lines: &[Line]) -> bool {
//!     lines.first().map_or(false, |l| l.l == MAGIC_LINE)
//! }
//!
//! // The fork bridge:
//! let replacements = line_diff(&a_lines, &b_lines);
//! ```

pub mod pair;
pub mod registry;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Top-level re-exports
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
    // Key constants — re-exported so nudox-ir-vcs/f1.rs can use them after the
    // local constants block is removed in favour of this crate.
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
