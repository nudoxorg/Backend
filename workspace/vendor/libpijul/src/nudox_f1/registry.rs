//! Frozen F1 key registry — single source of truth for both the serializer
//! (`nudox-ir-vcs`) and the field-aware pairer (`pair.rs`).
//!
//! The table is **version-1 only**. A future format bump (e.g. `NdIrF1\t2\n`)
//! will not match [`MAGIC_LINE`] and will record via stock Myers until this
//! crate grows a v2 registry — a safe, deterministic degradation.

// ---------------------------------------------------------------------------
// Magic line
// ---------------------------------------------------------------------------

/// The complete version-1 magic line, including its trailing newline, as a
/// `&str`.  Exists so the serializer in `nudox-ir-vcs` can call
/// `out.push_str(MAGIC_STR)` without any `from_utf8` dance.
pub const MAGIC_STR: &str = "NdIrF1\t1\n";

/// The same magic line as bytes.  This is the sniff constant: the fork
/// dispatches to field pairing iff a file's first `Line.l` is byte-equal
/// to this.  Exact equality is version-safe (an `NdIrF1\t2` blob will not
/// match) and chunk-safe (an 8192-byte binary chunk can never equal these
/// 9 bytes).
pub const MAGIC_LINE: &[u8] = MAGIC_STR.as_bytes();

// ---------------------------------------------------------------------------
// Key registry constants (frozen §6.2, moved from nudox-ir-vcs/f1.rs:40-74)
// ---------------------------------------------------------------------------

pub const KEY_MAGIC: &str = "NdIrF1";        // key 0 — magic header
pub const KEY_NAME: &str = "name";            // 1  scalar  E
pub const KEY_VIS: &str = "vis";              // 2  scalar  S
pub const KEY_KIND: &str = "kind";            // 3  scalar  S
pub const KEY_SPAN: &str = "span";            // 4  scalar
pub const KEY_SRC: &str = "src";              // 5  scalar
pub const KEY_PARENT: &str = "parent";        // 6  scalar
pub const KEY_CFG: &str = "cfg";              // 7  scalar  S
pub const KEY_ATTR: &str = "attr";            // 8  set     S
pub const KEY_DEPRECATED: &str = "deprecated"; // 9  scalar S
pub const KEY_ALIAS: &str = "alias";          // 10 set     E
pub const KEY_DOC: &str = "doc";              // 11 seq     E
pub const KEY_DLINK: &str = "dlink";          // 12 set
pub const KEY_RETGT: &str = "retgt";          // 13 scalar  S
pub const KEY_FNSIG: &str = "fnsig";          // 14 scalar  S
pub const KEY_GPARAM: &str = "gparam";        // 15 seq     S
pub const KEY_WHERE: &str = "where";          // 16 set     S
pub const KEY_IN: &str = "in";               // 17 seq     S E
pub const KEY_OUT: &str = "out";              // 18 seq     S E
pub const KEY_FIELDTY: &str = "fieldty";      // 19 scalar  S E
pub const KEY_RECFORM: &str = "recform";      // 20 scalar  S
pub const KEY_RECFIELD: &str = "recfield";    // 21 seq     S
pub const KEY_VFORM: &str = "vform";          // 22 scalar  S
pub const KEY_VDISCR: &str = "vdiscr";        // 23 scalar  S
pub const KEY_SUPER: &str = "super";          // 24 set     S
pub const KEY_TFLAGS: &str = "tflags";        // 25 scalar  S
pub const KEY_IOF: &str = "iof";              // 26 scalar  S
pub const KEY_IFOR: &str = "ifor";            // 27 scalar  S
pub const KEY_IFLAGS: &str = "iflags";        // 28 scalar  S
pub const KEY_CTY: &str = "cty";              // 29 scalar  S
pub const KEY_CVAL: &str = "cval";            // 30 scalar
pub const KEY_AUTO: &str = "auto";            // 31 set     S
pub const KEY_TYPE: &str = "type";            // 32 scalar  S E
pub const KEY_LINK: &str = "link";            // 33 set
pub const KEY_LFACT: &str = "lfact";          // 34 set     S

// ---------------------------------------------------------------------------
// Class enum
// ---------------------------------------------------------------------------

/// Diff class of an F1 key (§1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// 0 or 1 line; a value change is a rewrite (positional pairing).
    Scalar,
    /// 0..n lines, byte-sorted; members are independent, never rewrites
    /// (sorted merge-join).
    Set,
    /// 0..n lines, declaration order; position-respecting LCS (section-scoped
    /// Myers).
    Seq,
}

// ---------------------------------------------------------------------------
// KeySpec + REGISTRY
// ---------------------------------------------------------------------------

/// One entry in the frozen key registry.
pub struct KeySpec {
    pub name: &'static str,
    pub class: Class,
}

/// Keys 1–34 in frozen registry (= canonical emission) order, per §1.6.
///
/// Key 0 (the magic line, `NdIrF1`) is intentionally absent: `class_of` treats
/// it, like every unknown key, as [`Class::Seq`] — section-scoped Myers is the
/// safest total behaviour for content this table does not describe.
///
/// Counts: 21 scalar + 8 set + 5 seq = 34 field keys.
pub const REGISTRY: [KeySpec; 34] = [
    // 1
    KeySpec { name: KEY_NAME,       class: Class::Scalar },
    // 2
    KeySpec { name: KEY_VIS,        class: Class::Scalar },
    // 3
    KeySpec { name: KEY_KIND,       class: Class::Scalar },
    // 4
    KeySpec { name: KEY_SPAN,       class: Class::Scalar },
    // 5
    KeySpec { name: KEY_SRC,        class: Class::Scalar },
    // 6
    KeySpec { name: KEY_PARENT,     class: Class::Scalar },
    // 7
    KeySpec { name: KEY_CFG,        class: Class::Scalar },
    // 8
    KeySpec { name: KEY_ATTR,       class: Class::Set    },
    // 9
    KeySpec { name: KEY_DEPRECATED, class: Class::Scalar },
    // 10
    KeySpec { name: KEY_ALIAS,      class: Class::Set    },
    // 11
    KeySpec { name: KEY_DOC,        class: Class::Seq    },
    // 12
    KeySpec { name: KEY_DLINK,      class: Class::Set    },
    // 13
    KeySpec { name: KEY_RETGT,      class: Class::Scalar },
    // 14
    KeySpec { name: KEY_FNSIG,      class: Class::Scalar },
    // 15
    KeySpec { name: KEY_GPARAM,     class: Class::Seq    },
    // 16
    KeySpec { name: KEY_WHERE,      class: Class::Set    },
    // 17
    KeySpec { name: KEY_IN,         class: Class::Seq    },
    // 18
    KeySpec { name: KEY_OUT,        class: Class::Seq    },
    // 19
    KeySpec { name: KEY_FIELDTY,    class: Class::Scalar },
    // 20
    KeySpec { name: KEY_RECFORM,    class: Class::Scalar },
    // 21
    KeySpec { name: KEY_RECFIELD,   class: Class::Seq    },
    // 22
    KeySpec { name: KEY_VFORM,      class: Class::Scalar },
    // 23
    KeySpec { name: KEY_VDISCR,     class: Class::Scalar },
    // 24
    KeySpec { name: KEY_SUPER,      class: Class::Set    },
    // 25
    KeySpec { name: KEY_TFLAGS,     class: Class::Scalar },
    // 26
    KeySpec { name: KEY_IOF,        class: Class::Scalar },
    // 27
    KeySpec { name: KEY_IFOR,       class: Class::Scalar },
    // 28
    KeySpec { name: KEY_IFLAGS,     class: Class::Scalar },
    // 29
    KeySpec { name: KEY_CTY,        class: Class::Scalar },
    // 30
    KeySpec { name: KEY_CVAL,       class: Class::Scalar },
    // 31
    KeySpec { name: KEY_AUTO,       class: Class::Set    },
    // 32
    KeySpec { name: KEY_TYPE,       class: Class::Scalar },
    // 33
    KeySpec { name: KEY_LINK,       class: Class::Set    },
    // 34
    KeySpec { name: KEY_LFACT,      class: Class::Set    },
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the key of a raw F1 line: the bytes before the **first** TAB.
/// Values may contain further TABs (`span\t<start>\t<end>`), so only the first
/// TAB is significant.  A line with no TAB is returned as its own key.
#[inline]
pub fn key_of(line: &[u8]) -> &[u8] {
    match line.iter().position(|&b| b == b'\t') {
        Some(i) => &line[..i],
        None    => line,
    }
}

/// Class of a key.
///
/// Unknown keys — the magic line (`NdIrF1`), malformed lines, future registry
/// additions — diff as [`Class::Seq`]: position-respecting LCS is the safest
/// total behaviour for content this table does not describe.
pub fn class_of(key: &[u8]) -> Class {
    for spec in REGISTRY.iter() {
        if spec.name.as_bytes() == key {
            return spec.class;
        }
    }
    Class::Seq
}
