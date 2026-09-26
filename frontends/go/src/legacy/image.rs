//! Validates the fixed Go semantic-authority image emitted by `go/packages`.
//! Keeps every semantic plane — module metadata, package rows, declarations,
//! recursive type rows, signature parameters, methods, type parameters,
//! struct/interface members, complete interface method sets, documentation,
//! references, and build constraints — borrowed from one checksummed binary
//! format. Rejects malformed or source-mismatched images before compiler
//! admission.
//!
//! ## Format version 5 (zero-copy, fixed-width rows)
//!
//! Header (136 bytes): magic `NGAI`, version `5`, header length, declaration
//! count, atom-plane byte length, body byte length, SHA-256 source digest,
//! SHA-256 checksum, then eight plane row counts (types, references, methods,
//! type parameters, members, docs, build constraints, satisfactions), the
//! module count, the package count, the signature-parameter count, and the
//! method-set count, and the unresolved-cgo count (version 6; zero keeps
//! every prior image byte-identical in the body).
//!
//! Body planes, in order: declarations (56 B rows), type rows (52 B),
//! methods (64 B), type parameters (16 B), members — struct fields and
//! interface method signatures (40 B), documentation rows (16 B), references
//! (48 B), build constraints (28 B), satisfaction rows (20 B), the single
//! module row (32 B), package rows (28 B), signature-parameter rows (28 B),
//! interface method-set rows (24 B), pooled type children (8 B), and the
//! shared UTF-8 atom plane. Every range cell is validated against its plane;
//! the pooled child plane must tile each type row's declared child run
//! exactly; member runs must match the type rows that declare them;
//! documentation, reference, constraint, and satisfaction rows must be
//! canonically ordered; every reference owner must resolve to a function
//! declaration or a method row whose file and span contain the call site;
//! every satisfaction subject must be a named-type declaration. A func type
//! row's parameter count cell splits its child run into the leading
//! parameters and the trailing results; every other kind must leave that
//! cell zero.
//!
//! Version 5 carries the oracle's complete `Output`. The module row owns the
//! `go.mod` metadata (path, directory, Go directive, resolved version) and is
//! present exactly when the oracle resolved a module. Package rows own each
//! package's import path, package clause, and source file list, canonically
//! ordered by import path; every declaration must name one of the package
//! rows' import paths. Signature-parameter rows own the exact source name
//! and source position of every parameter and result of every func type row:
//! one row per child, owner-contiguous in type-row order, ordinals ascending
//! from zero, the plane exactly tiling the func rows' child runs (empty names
//! are legal — Go permits unnamed parameters and results — and a file-less
//! position must be fully absent). Method-set rows own the complete
//! post-embedding method set of every interface type row — a fact that is
//! not locally derivable when an embedded interface declares in another
//! package — owner-contiguous in type-row order, names strictly ascending
//! within one owner, every owner an interface row. The type row layout is
//! unchanged from version 4: its reserved bytes stay reserved. Line and
//! column positions stay off the wire: they are losslessly derivable from
//! the source digest plus the byte offsets already carried.
//!
//! Since version 4, constant declarations own their exact value atom,
//! const-group identity, and iota flag on the declaration row,
//! interface-satisfaction edges own the satisfaction plane, and
//! package-level doc comments own the `Package` documentation kind.
//!
//! ## Format version 6 (additive widenings over version 5)
//!
//! Version 6 keeps the header shape (version cell `6`), the plane order, and
//! every untouched row width, and widens three rows additively. The reader
//! accepts BOTH versions; a version-5 image yields the version-6 facts as
//! their absent defaults (no name extent, not bound, call-kind free-func
//! references with no receiver type), so every v5 producer stays readable
//! while v6 producers carry:
//!
//!   - declaration rows 56 → 72 bytes: the declared identifier's exact byte
//!     extent (`nameStart`/`nameEnd` at [56..64) — both NONE or both
//!     present, ordered, and contained in the row's span when one exists)
//!     plus the one-byte authority-bound flag at [64] (`0`/`1`; `1` marks a
//!     declaration that declares in the exact source file the image is
//!     digest-bound to, so the lowerer can attach primary-source spans
//!     without comparing path spellings across processes). Bytes [65..72)
//!     stay reserved and must be zero.
//!   - method rows 64 → 72 bytes: the same one-byte bound flag at [64];
//!     bytes [65..72) stay reserved and must be zero.
//!   - reference rows 48 → 56 bytes: the closed use-kind byte at [44]
//!     (0 call, 1 read, 2 typeref, 3 import), the closed used-object class
//!     byte at [45] (0 func, 1 method, 2 field, 3 var, 4 const, 5 type,
//!     6 pkg), bytes [46..48) reserved and zero, and the target receiver
//!     type-name atom at [48..56) — empty for every class but method and
//!     field, whose same-package targets the lowerer keys through it.

use core::str;

use sha2::{Digest, Sha256};
use thiserror::Error;

const MAGIC: [u8; 4] = *b"NGAI";
/// The image versions this reader admits: version 5, the complete
/// zero-copy layout, and version 6, which widens three rows additively
/// (declaration name extents and bound flags; typed reference rows).
const VERSION: u16 = 6;
const SUPPORTED_VERSIONS: [u16; 2] = [5, 6];
const HEADER_BYTES: usize = 136;
const MODULE_BYTES: usize = 32;
const PACKAGE_BYTES: usize = 28;
/// Declaration row width by image version: version 6 appends the declared
/// identifier's byte extent (8 bytes) and the one-byte bound flag (+7
/// reserved).
const DECLARATION_BYTES_V5: usize = 56;
const DECLARATION_BYTES_V6: usize = 72;
const TYPE_ROW_BYTES: usize = 52;
const SIGNATURE_PARAMETER_BYTES: usize = 28;
/// Method row width by image version: version 6 appends the one-byte bound
/// flag (+7 reserved).
const METHOD_BYTES_V5: usize = 64;
const METHOD_BYTES_V6: usize = 72;
const TYPE_PARAMETER_BYTES: usize = 16;
const MEMBER_BYTES: usize = 40;
const METHOD_SET_BYTES: usize = 24;
const DOC_BYTES: usize = 16;
/// Reference row width by image version: version 6 reuses the reserved tail
/// for the closed use-kind and class bytes and appends the receiver type
/// atom.
const REFERENCE_BYTES_V5: usize = 48;
const REFERENCE_BYTES_V6: usize = 56;
const CONSTRAINT_BYTES: usize = 28;
const SATISFACTION_BYTES: usize = 20;
const CHILD_BYTES: usize = 8;
/// The domain-separated checksum input by image version.
const DIGEST_DOMAIN_V5: &[u8] = b"nudox.go.authority.image.sha256.v5\0";
const DIGEST_DOMAIN_V6: &[u8] = b"nudox.go.authority.image.sha256.v6\0";

/// The `u32::MAX` sentinel shared by every optional coordinate cell.
pub const NONE: u32 = u32::MAX;

/// A closed declaration classification supplied by the Go type authority.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A defined named type from the package scope.
    Type = 1,
    /// A true type alias from the package scope.
    Alias = 2,
    /// A package-level function.
    Function = 3,
    /// A package-level constant.
    Constant = 4,
    /// A package-level variable.
    Static = 5,
}

impl DeclarationKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Type),
            2 => Some(Self::Alias),
            3 => Some(Self::Function),
            4 => Some(Self::Constant),
            5 => Some(Self::Static),
            _ => None,
        }
    }
}

/// The closed type-row discriminants of the recursive type plane. Frozen;
/// mirrors the oracle's `TypeKind` JSON enum.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeRowKind {
    /// A basic type (`int`, `byte`, `unsafe.Pointer`, …); the name is required.
    Basic = 0,
    /// A defined named type by reference (`pkg` + `name` + type arguments).
    Named = 1,
    /// A true alias by reference.
    Alias = 2,
    /// A use of a generic type parameter by name.
    TypeParam = 3,
    /// A pointer; the element is the row's one child.
    Pointer = 4,
    /// A slice; the element is the row's one child.
    Slice = 5,
    /// A fixed-length array; the element is the row's one child and the
    /// length cell owns the size.
    Array = 6,
    /// A map; children are `[key, value]`.
    Map = 7,
    /// A channel; the element is the row's one child and the direction cell
    /// owns `chan` / `chan<-` / `<-chan`.
    Chan = 8,
    /// A function; children are parameters followed by results.
    Func = 9,
    /// A struct; members own the fields and the row owns no children.
    Struct = 10,
    /// An interface; children are the embeddeds and members the explicit
    /// method signatures.
    Interface = 11,
    /// A constraint union; children are the term types (child flag bit 0 is
    /// the tilde marker).
    Union = 12,
    /// A tuple; children are the component types.
    Tuple = 13,
    /// The oracle's honest unknown shape.
    Invalid = 14,
}

impl TypeRowKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Basic),
            1 => Some(Self::Named),
            2 => Some(Self::Alias),
            3 => Some(Self::TypeParam),
            4 => Some(Self::Pointer),
            5 => Some(Self::Slice),
            6 => Some(Self::Array),
            7 => Some(Self::Map),
            8 => Some(Self::Chan),
            9 => Some(Self::Func),
            10 => Some(Self::Struct),
            11 => Some(Self::Interface),
            12 => Some(Self::Union),
            13 => Some(Self::Tuple),
            14 => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// Channel direction cell values.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChanDir {
    /// An unrestricted `chan`.
    Both = 0,
    /// A send-only `chan<-`.
    Send = 1,
    /// A receive-only `<-chan`.
    Recv = 2,
}

/// One borrowed module-metadata row: the `go.mod` facts of the loaded
/// module. The plane holds exactly one row; cells are empty when the oracle
/// resolved no module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleRow<'image> {
    /// The module path (the `module` directive).
    pub path: &'image [u8],
    /// The module's on-disk root directory.
    pub directory: &'image [u8],
    /// The `go` directive spelling (e.g. `1.22`).
    pub go_version: &'image [u8],
    /// The resolved module version; empty for a working-tree module.
    pub version: &'image [u8],
}

/// One borrowed package row: one Go package of the module with its source
/// files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageRow<'image> {
    /// The fully-qualified import path.
    pub import_path: &'image [u8],
    /// The package clause identifier.
    pub name: &'image [u8],
    /// Source file spellings, NUL-separated (validated at open).
    pub files: &'image [u8],
    /// Number of source files in [`PackageRow::files`].
    pub file_count: u32,
}

/// One borrowed Go declaration from the checked authority image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Declaration<'image> {
    /// The declaration's closed Go kind.
    pub kind: DeclarationKind,
    /// Whether the package scope marks this identifier exported.
    pub exported: bool,
    /// Whether the constant block this declaration belongs to uses `iota`
    /// (constants only; `false` elsewhere).
    pub iota: bool,
    /// Exact UTF-8 identifier bytes lent from the image atom plane.
    pub name: &'image [u8],
    /// The declaring package's import path (empty for none).
    pub package: &'image [u8],
    /// The declaration's type-row root, when the authority spelled one.
    pub type_root: Option<u32>,
    /// The absolute file byte range of the declaration's full source text,
    /// when the authority resolved one.
    pub span: Option<(u32, u32)>,
    /// The absolute file byte range of the declaration's own identifier
    /// token (version 6; `None` on version-5 images). Contained in `span`
    /// whenever both are present.
    pub name_span: Option<(u32, u32)>,
    /// Whether the declaration declares in the exact source file the image
    /// is digest-bound to (version 6; `false` on version-5 images). Only
    /// bound rows may contribute primary-source spans.
    pub bound: bool,
    /// The span's source file spelling.
    pub file: &'image [u8],
    /// The exact constant value (`constant.Value.ExactString`; constants
    /// only, empty elsewhere).
    pub value: &'image [u8],
    /// The const declaration block this constant belongs to (`0` for none).
    pub const_group: i64,
}

/// One borrowed recursive type row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeRow<'image> {
    /// The row's closed discriminant.
    pub kind: TypeRowKind,
    /// Channel direction (chan rows only; [`ChanDir::Both`] elsewhere).
    pub dir: ChanDir,
    /// Whether a func row's final parameter is variadic.
    pub variadic: bool,
    /// Basic / named / alias / type-parameter name bytes.
    pub name: &'image [u8],
    /// Defining package import path (empty for universe names).
    pub package: &'image [u8],
    /// Fixed array length (array rows only).
    pub length: i64,
    /// Leading child count owned by parameters (func rows only; the
    /// remaining children are results). Every other kind carries zero.
    pub param_count: u32,
    /// Ordered pooled child run `[start, start + count)`.
    pub children: (u32, u32),
    /// Ordered member run `[start, start + count)`.
    pub members: (u32, u32),
}

/// One borrowed method row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MethodRow<'image> {
    /// Owning declaration index (the receiver's named type).
    pub owner: u32,
    /// Whether the method name is exported.
    pub exported: bool,
    /// `true` for `func (t *T)`, `false` for `func (t T)`.
    pub pointer_receiver: bool,
    /// `true` for methods promoted through embedded fields.
    pub promoted: bool,
    /// Method identifier bytes.
    pub name: &'image [u8],
    /// Signature type-row root, when spelled.
    pub type_root: Option<u32>,
    /// Receiver binding name bytes (`s` in `(s *Server)`).
    pub receiver: &'image [u8],
    /// Receiver type-parameter names, NUL-separated (validated at open).
    pub receiver_type_params: &'image [u8],
    /// Promoted-method origin spelling (`sync.Mutex`); empty when declared.
    pub origin: &'image [u8],
    /// Absolute file byte range of the declaring `func` decl, when resolved.
    pub span: Option<(u32, u32)>,
    /// Whether the method declares in the exact source file the image is
    /// digest-bound to (version 6; `false` on version-5 images).
    pub bound: bool,
    /// The declaring source file spelling.
    pub file: &'image [u8],
}

/// One borrowed type-parameter row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeParameterRow<'image> {
    /// Owning declaration index.
    pub owner: u32,
    /// Parameter name bytes.
    pub name: &'image [u8],
    /// Constraint type-row root, when the source wrote one.
    pub constraint: Option<u32>,
}

/// One borrowed signature-parameter row: the exact source name and source
/// position of one parameter or result of one func type row. Names may be
/// empty — Go permits unnamed parameters and results — and a nameless,
/// positionless row is legal whenever the source wrote neither.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureParameterRow<'image> {
    /// Owning func type-row index.
    pub owner: u32,
    /// Position inside the owner's child run: parameters first, then
    /// results, ordinals ascending from zero.
    pub ordinal: u32,
    /// Exact source name bytes; empty when the source wrote none.
    pub name: &'image [u8],
    /// Source file spelling of the parameter identifier; empty when the
    /// position was never resolved.
    pub file: &'image [u8],
    /// Byte offset of the identifier within [`SignatureParameterRow::file`];
    /// [`NONE`] when the position was never resolved.
    pub offset: u32,
}

/// What kind of member a member row carries.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberKind {
    /// A struct field.
    Field = 0,
    /// An interface method signature.
    Method = 1,
}

/// One borrowed member row (struct field or interface method signature).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemberRow<'image> {
    /// Owning type-row index.
    pub owner: u32,
    /// The member's closed kind.
    pub kind: MemberKind,
    /// Whether the field is anonymous/embedded (struct fields only).
    pub embedded: bool,
    /// Whether the member name is exported.
    pub exported: bool,
    /// Member name bytes (the implicit type name for embedded fields).
    pub name: &'image [u8],
    /// Member type-row root, when spelled.
    pub type_root: Option<u32>,
    /// Raw struct tag bytes (struct fields only; empty otherwise).
    pub tag: &'image [u8],
    /// Declaring package import path (interface methods; empty otherwise).
    pub package: &'image [u8],
}

/// One borrowed interface method-set row: one method of an interface type
/// row's complete post-embedding method set. Inherited methods from
/// embedded interfaces — including foreign-package embeddeds whose
/// declarations the image carries only by reference — appear here beside
/// the explicitly declared ones.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MethodSetRow<'image> {
    /// Owning interface type-row index.
    pub owner: u32,
    /// Method identifier bytes.
    pub name: &'image [u8],
    /// Signature type-row root, when the authority spelled one.
    pub type_root: Option<u32>,
    /// Import path of the package that declared the method; empty when it
    /// declares in the interface's own package or the universe.
    pub package: &'image [u8],
}

/// Which lane owns one documentation row.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocOwner {
    /// A declaration row.
    Declaration = 0,
    /// A method row.
    Method = 1,
    /// A member row.
    Member = 2,
    /// One package's doc comment; the owner cell names the package's first
    /// declaration.
    Package = 3,
}

/// One borrowed interface-satisfaction row: a named type declaration that
/// satisfies a non-empty interface named by reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SatisfactionRow<'image> {
    /// The satisfying named-type declaration index.
    pub subject: u32,
    /// The satisfied interface's bare name.
    pub target: &'image [u8],
    /// The satisfied interface's defining package import path; empty when
    /// the interface declares in the subject's own package.
    pub target_package: &'image [u8],
}

/// One borrowed documentation row: cleaned doc prose for one owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocRow<'image> {
    /// The owning lane.
    pub owner_kind: DocOwner,
    /// The owner's row index inside its lane.
    pub owner: u32,
    /// The cleaned doc text (markers and directives already stripped).
    pub text: &'image [u8],
}

/// One borrowed reference row: a compiler-resolved use of one named object
/// with its owner-relative span proven at validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRow<'image> {
    /// Owning declaration index (the enclosing declaration).
    pub owner: u32,
    /// Used object's bare name.
    pub target: &'image [u8],
    /// Target package import path; empty for a same-package target.
    pub target_package: &'image [u8],
    /// Bare receiver type name; empty when the enclosing declaration is a
    /// package-level function.
    pub receiver: &'image [u8],
    /// Absolute `[start, end)` byte span of the used identifier token — the
    /// NAME-TOKEN extent.
    pub span: (u32, u32),
    /// Source file spelling of the use.
    pub file: &'image [u8],
    /// `true` when the enclosing declaration is the declaration itself;
    /// `false` when it is the method row at `owner_row`.
    pub owner_is_declaration: bool,
    /// Resolved enclosing row: the declaration index, or the method row
    /// index.
    pub owner_row: u32,
    /// The use span relative to the resolved owner's span start.
    pub relative: (u32, u32),
    /// The use's closed kind (version 6; [`ReferenceUseKind::Call`] on
    /// version-5 images, which record only calls).
    pub use_kind: ReferenceUseKind,
    /// The used object's closed class (version 6;
    /// [`ReferenceTargetClass::Func`] on version-5 images).
    pub target_class: ReferenceTargetClass,
    /// The target's receiver type name for method and field targets;
    /// empty otherwise (version 6; empty on version-5 images).
    pub recv_type: &'image [u8],
}

/// The closed use-kind vocabulary of a reference row.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceUseKind {
    /// A call: the identifier is the called position of a call expression.
    Call = 0,
    /// A value use — reads and writes alike; go/types' Uses table records
    /// no lvalue distinction.
    Read = 1,
    /// A use of a named type in type position.
    TypeRef = 2,
    /// A use of an imported package binding.
    Import = 3,
}

impl ReferenceUseKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Call),
            1 => Some(Self::Read),
            2 => Some(Self::TypeRef),
            3 => Some(Self::Import),
            _ => None,
        }
    }
}

/// The closed used-object class vocabulary of a reference row.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTargetClass {
    /// A free (receiver-less) package-level function.
    Func = 0,
    /// A method.
    Method = 1,
    /// A struct field.
    Field = 2,
    /// A package-level variable.
    Var = 3,
    /// A package-level constant.
    Const = 4,
    /// A named type or alias.
    Type = 5,
    /// An imported package binding.
    Pkg = 6,
}

impl ReferenceTargetClass {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Func),
            1 => Some(Self::Method),
            2 => Some(Self::Field),
            3 => Some(Self::Var),
            4 => Some(Self::Const),
            5 => Some(Self::Type),
            6 => Some(Self::Pkg),
            _ => None,
        }
    }
}

/// One borrowed build-constraint row: one excluded source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConstraintRow<'image> {
    /// The excluded source file.
    pub file: &'image [u8],
    /// The normalized constraint expression (`windows`).
    pub constraint: &'image [u8],
    /// The exported declaration blob: `(kind byte, name, 0x00)` records.
    pub exported: &'image [u8],
    /// Number of exported declaration records in the blob.
    pub exported_count: u32,
}

/// One parsed exported declaration from a build-constraint blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConstrainedDecl<'image> {
    /// The declaration's closed kind.
    pub kind: DeclarationKind,
    /// The declaration's name bytes.
    pub name: &'image [u8],
}

/// A validated immutable Go authority image borrowing caller-owned bytes.
#[derive(Clone, Copy, Debug)]
pub struct GoImage<'image> {
    bytes: &'image [u8],
    version: u16,
    declaration_bytes: usize,
    method_bytes: usize,
    reference_bytes: usize,
    digest_domain: &'static [u8],
    module_count: usize,
    package_count: usize,
    declaration_count: usize,
    type_count: usize,
    signature_parameter_count: usize,
    method_count: usize,
    type_parameter_count: usize,
    member_count: usize,
    method_set_count: usize,
    doc_count: usize,
    reference_count: usize,
    constraint_count: usize,
    satisfaction_count: usize,
    child_count: usize,
    unresolved_cgo_count: usize,
    packages_offset: usize,
    declarations_offset: usize,
    types_offset: usize,
    signature_parameters_offset: usize,
    methods_offset: usize,
    type_parameters_offset: usize,
    members_offset: usize,
    method_sets_offset: usize,
    docs_offset: usize,
    references_offset: usize,
    constraints_offset: usize,
    satisfactions_offset: usize,
    module_offset: usize,
    children_offset: usize,
    cgo_offset: usize,
    atom_offset: usize,
    atom_bytes: usize,
    source_digest: [u8; 32],
}

impl<'image> GoImage<'image> {
    /// Opens one complete fixed-layout Go authority image and proves every
    /// structural law before lending any row.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let found_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if found_magic != MAGIC {
            return Err(ImageError::Header(HeaderError::Magic {
                found: found_magic,
            }));
        }
        let version = u16_at(bytes, 4);
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(ImageError::Header(HeaderError::Version { found: version }));
        }
        let (declaration_bytes, method_bytes, reference_bytes, digest_domain) = if version == 6 {
            (
                DECLARATION_BYTES_V6,
                METHOD_BYTES_V6,
                REFERENCE_BYTES_V6,
                DIGEST_DOMAIN_V6,
            )
        } else {
            (
                DECLARATION_BYTES_V5,
                METHOD_BYTES_V5,
                REFERENCE_BYTES_V5,
                DIGEST_DOMAIN_V5,
            )
        };
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let actual_body = bytes.len() - HEADER_BYTES;
        let declaration_count = plane_count(bytes, 8, actual_body)?;
        let atom_bytes = plane_count(bytes, 12, actual_body)?;
        let body_bytes = plane_count(bytes, 16, actual_body)?;
        let type_count = plane_count(bytes, 84, actual_body)?;
        let reference_count = plane_count(bytes, 88, actual_body)?;
        let method_count = plane_count(bytes, 92, actual_body)?;
        let type_parameter_count = plane_count(bytes, 96, actual_body)?;
        let member_count = plane_count(bytes, 100, actual_body)?;
        let doc_count = plane_count(bytes, 104, actual_body)?;
        let constraint_count = plane_count(bytes, 108, actual_body)?;
        let satisfaction_count = plane_count(bytes, 112, actual_body)?;
        let module_count = plane_count(bytes, 116, actual_body)?;
        if module_count > 1 {
            return Err(ImageError::Header(HeaderError::ModuleCount {
                found: module_count,
            }));
        }
        let package_count = plane_count(bytes, 120, actual_body)?;
        let signature_parameter_count = plane_count(bytes, 124, actual_body)?;
        let method_set_count = plane_count(bytes, 128, actual_body)?;
        let unresolved_cgo_count = plane_count(bytes, 132, actual_body)?;

        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[20..52]);

        // Body planes, in frozen order: declarations, types, methods, type
        // parameters, members, docs, references, constraints, satisfactions,
        // module, packages, signature parameters, interface method sets,
        // pooled children, unresolved-cgo cells, atoms.
        let declarations_offset = HEADER_BYTES;
        let types_offset = declarations_offset + declaration_count * declaration_bytes;
        let methods_offset = types_offset + type_count * TYPE_ROW_BYTES;
        let type_parameters_offset = methods_offset + method_count * method_bytes;
        let members_offset = type_parameters_offset + type_parameter_count * TYPE_PARAMETER_BYTES;
        let docs_offset = members_offset + member_count * MEMBER_BYTES;
        let references_offset = docs_offset + doc_count * DOC_BYTES;
        let constraints_offset = references_offset + reference_count * reference_bytes;
        let satisfactions_offset = constraints_offset + constraint_count * CONSTRAINT_BYTES;
        let module_offset = satisfactions_offset + satisfaction_count * SATISFACTION_BYTES;
        let packages_offset = module_offset + module_count * MODULE_BYTES;
        let signature_parameters_offset = packages_offset + package_count * PACKAGE_BYTES;
        let method_sets_offset =
            signature_parameters_offset + signature_parameter_count * SIGNATURE_PARAMETER_BYTES;
        let children_offset = method_sets_offset + method_set_count * METHOD_SET_BYTES;
        let Some(atoms_end) = HEADER_BYTES.checked_add(body_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        if atoms_end != bytes.len() || atoms_end < children_offset {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }
        let cgo_plane_bytes = unresolved_cgo_count
            .checked_mul(CHILD_BYTES)
            .ok_or(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }))?;
        let Some(atom_offset) = atoms_end.checked_sub(atom_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        let Some(cgo_offset) = atom_offset.checked_sub(cgo_plane_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        if cgo_offset < children_offset
            || !(cgo_offset - children_offset).is_multiple_of(CHILD_BYTES)
        {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }
        let child_count = (cgo_offset - children_offset) / CHILD_BYTES;
        // Every fixed plane must sit inside the children plane origin, so no
        // declared count can push a row read past the validated body.
        let chain = [
            declarations_offset,
            types_offset,
            methods_offset,
            type_parameters_offset,
            members_offset,
            docs_offset,
            references_offset,
            constraints_offset,
            satisfactions_offset,
            module_offset,
            packages_offset,
            signature_parameters_offset,
            method_sets_offset,
        ];
        if chain.iter().any(|offset| *offset > atom_offset) {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }

        let image = Self {
            bytes,
            version,
            declaration_bytes,
            method_bytes,
            reference_bytes,
            digest_domain,
            module_count,
            package_count,
            declaration_count,
            type_count,
            signature_parameter_count,
            method_count,
            type_parameter_count,
            member_count,
            method_set_count,
            doc_count,
            reference_count,
            constraint_count,
            satisfaction_count,
            child_count,
            unresolved_cgo_count,
            declarations_offset,
            types_offset,
            methods_offset,
            type_parameters_offset,
            members_offset,
            docs_offset,
            references_offset,
            constraints_offset,
            satisfactions_offset,
            module_offset,
            packages_offset,
            signature_parameters_offset,
            method_sets_offset,
            children_offset,
            cgo_offset,
            atom_offset,
            atom_bytes,
            source_digest,
        };
        image.validate_digest()?;
        image.validate_module()?;
        image.validate_declarations()?;
        image.validate_packages()?;
        image.validate_types()?;
        image.validate_methods()?;
        image.validate_type_parameters()?;
        image.validate_members()?;
        image.validate_method_sets()?;
        image.validate_docs()?;
        image.validate_references()?;
        image.validate_constraints()?;
        image.validate_satisfactions()?;
        Ok(image)
    }

    /// Returns the SHA-256 digest of the exact configured Go source file.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Number of validated declarations.
    #[must_use]
    pub const fn declaration_count(self) -> usize {
        self.declaration_count
    }

    /// Number of validated type rows.
    #[must_use]
    pub const fn type_count(self) -> usize {
        self.type_count
    }

    /// Number of validated method rows.
    #[must_use]
    pub const fn method_count(self) -> usize {
        self.method_count
    }

    /// Number of validated member rows.
    #[must_use]
    pub const fn member_count(self) -> usize {
        self.member_count
    }

    /// Number of validated type-parameter rows.
    #[must_use]
    pub const fn type_parameter_count(self) -> usize {
        self.type_parameter_count
    }

    /// Number of validated documentation rows.
    #[must_use]
    pub const fn doc_count(self) -> usize {
        self.doc_count
    }

    /// Number of validated reference rows.
    #[must_use]
    pub const fn reference_count(self) -> usize {
        self.reference_count
    }

    /// Number of validated build-constraint rows.
    #[must_use]
    pub const fn constraint_count(self) -> usize {
        self.constraint_count
    }

    /// Number of validated satisfaction rows.
    #[must_use]
    pub const fn satisfaction_count(self) -> usize {
        self.satisfaction_count
    }

    /// Number of unresolved-cgo name cells carried before the atom plane.
    #[must_use]
    pub const fn unresolved_cgo_count(self) -> usize {
        self.unresolved_cgo_count
    }

    /// Borrows one unresolved-cgo name atom by index.
    pub fn unresolved_cgo(self, index: usize) -> Result<&'image [u8], ImageError> {
        if index >= self.unresolved_cgo_count {
            return Err(ImageError::RowBounds {
                plane: "unresolved cgo",
                index,
                count: self.unresolved_cgo_count,
            });
        }
        let row = self.plane_row(self.cgo_offset, index, CHILD_BYTES);
        self.atom("unresolved cgo", index, u32_at(row, 0), u32_at(row, 4))
    }

    /// Borrows one validated declaration row.
    pub fn declaration(self, index: usize) -> Result<Declaration<'image>, ImageError> {
        if index >= self.declaration_count {
            return Err(ImageError::RowBounds {
                plane: "declaration",
                index,
                count: self.declaration_count,
            });
        }
        let row = self.plane_row(self.declarations_offset, index, self.declaration_bytes);
        let kind = DeclarationKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
        })?;
        let exported = flag_at(row, 1).ok_or(ImageError::ExportedFlag {
            index,
            found: row[1],
        })?;
        let iota = flag_at(row, 2).ok_or(ImageError::DeclarationIota {
            index,
            found: row[2],
        })?;
        if row[3] != 0 {
            return Err(ImageError::DeclarationReserved { index });
        }
        let name = self.atom("declaration", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "declaration",
                index,
            });
        }
        let package = self.atom("declaration", index, u32_at(row, 12), u32_at(row, 16))?;
        let type_root = optional_row(u32_at(row, 20));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::DeclarationTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let span = span_at(row, 24, 28, index)?;
        let file = self.atom("declaration", index, u32_at(row, 32), u32_at(row, 36))?;
        let value = self.atom("declaration", index, u32_at(row, 40), u32_at(row, 44))?;
        let const_group = i64::from_le_bytes([
            row[48], row[49], row[50], row[51], row[52], row[53], row[54], row[55],
        ]);
        // Version 6 tail: the declared identifier's byte extent and the
        // authority-bound flag. Version 5 rows carry neither fact.
        let (name_span, bound) = if self.version == 6 {
            let name_span = span_at(row, 56, 60, index)?;
            if let (Some((span_start, span_end)), Some((name_start, name_end))) = (span, name_span)
                && (name_start < span_start || name_end > span_end)
            {
                // The identifier must sit inside its own declaration: a
                // name extent escaping the span could never anchor an
                // owner-relative occurrence inside the owner.
                return Err(ImageError::DeclarationSpan {
                    index,
                    start: name_start,
                    end: name_end,
                });
            }
            let bound = flag_at(row, 64).ok_or(ImageError::DeclarationReserved { index })?;
            if row[65..72] != [0; 7] {
                return Err(ImageError::DeclarationReserved { index });
            }
            (name_span, bound)
        } else {
            // Version 5 rows end at the const-group cell: no v6 facts exist.
            (None, false)
        };
        Ok(Declaration {
            kind,
            exported,
            iota,
            name,
            package,
            type_root,
            span,
            name_span,
            bound,
            file,
            value,
            const_group,
        })
    }

    /// Iterates all package-scope declarations in producer order.
    pub fn declarations(self) -> impl Iterator<Item = Result<Declaration<'image>, ImageError>> {
        (0..self.declaration_count).map(move |index| self.declaration(index))
    }

    /// Borrows one validated type row.
    pub fn type_row(self, index: usize) -> Result<TypeRow<'image>, ImageError> {
        if index >= self.type_count {
            return Err(ImageError::RowBounds {
                plane: "type",
                index,
                count: self.type_count,
            });
        }
        let row = self.plane_row(self.types_offset, index, TYPE_ROW_BYTES);
        let kind = TypeRowKind::decode(row[0]).ok_or(ImageError::TypeKind {
            index,
            found: row[0],
        })?;
        let dir = match row[1] {
            0 => ChanDir::Both,
            1 => ChanDir::Send,
            2 => ChanDir::Recv,
            found => return Err(ImageError::TypeDirection { index, found }),
        };
        let variadic = flag_at(row, 2).ok_or(ImageError::TypeVariadicFlag {
            index,
            found: row[2],
        })?;
        if row[3] != 0 || row[44..48] != [0; 4] {
            return Err(ImageError::TypeReserved { index });
        }
        let name = self.atom("type", index, u32_at(row, 4), u32_at(row, 8))?;
        let package = self.atom("type", index, u32_at(row, 12), u32_at(row, 16))?;
        let length = i64::from_le_bytes([
            row[20], row[21], row[22], row[23], row[24], row[25], row[26], row[27],
        ]);
        Ok(TypeRow {
            kind,
            dir,
            variadic,
            name,
            package,
            length,
            param_count: u32_at(row, 48),
            children: (u32_at(row, 28), u32_at(row, 32)),
            members: (u32_at(row, 36), u32_at(row, 40)),
        })
    }

    /// Borrows one validated pooled type child: target row plus term flags.
    pub fn type_child(self, index: usize) -> Result<(u32, bool), ImageError> {
        if index >= self.child_count {
            return Err(ImageError::RowBounds {
                plane: "type child",
                index,
                count: self.child_count,
            });
        }
        let row = self.plane_row(self.children_offset, index, CHILD_BYTES);
        let target = u32_at(row, 0);
        if usize::try_from(target).is_ok_and(|target| target >= self.type_count) {
            return Err(ImageError::TypeChildTarget {
                index,
                target,
                type_count: self.type_count,
            });
        }
        let flags = u32_at(row, 4);
        if flags & !CHILD_TILDE_FLAG != 0 {
            return Err(ImageError::TypeChildFlags { index, flags });
        }
        Ok((target, flags & CHILD_TILDE_FLAG == CHILD_TILDE_FLAG))
    }

    /// Borrows one validated method row.
    pub fn method(self, index: usize) -> Result<MethodRow<'image>, ImageError> {
        if index >= self.method_count {
            return Err(ImageError::RowBounds {
                plane: "method",
                index,
                count: self.method_count,
            });
        }
        let row = self.plane_row(self.methods_offset, index, self.method_bytes);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.declaration_count) {
            return Err(ImageError::MethodOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let exported = flag_at(row, 4).ok_or(ImageError::MethodFlag {
            index,
            cell: "exported",
            found: row[4],
        })?;
        let pointer_receiver = flag_at(row, 5).ok_or(ImageError::MethodFlag {
            index,
            cell: "pointer-receiver",
            found: row[5],
        })?;
        let promoted = flag_at(row, 6).ok_or(ImageError::MethodFlag {
            index,
            cell: "promoted",
            found: row[6],
        })?;
        if row[7] != 0 {
            return Err(ImageError::MethodReserved { index });
        }
        let name = self.atom("method", index, u32_at(row, 8), u32_at(row, 12))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "method",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 16));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MethodTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let receiver = self.atom("method", index, u32_at(row, 20), u32_at(row, 24))?;
        let blob_length = u32_at(row, 32);
        let receiver_type_params = self.atom("method", index, u32_at(row, 28), blob_length)?;
        let param_count = u32_at(row, 36);
        if split_nul(receiver_type_params, param_count).is_none() {
            return Err(ImageError::MethodReceiverParams {
                index,
                count: param_count,
                blob_bytes: blob_length,
            });
        }
        let origin = self.atom("method", index, u32_at(row, 40), u32_at(row, 44))?;
        let span = span_at(row, 48, 52, index)?;
        let file = self.atom("method", index, u32_at(row, 56), u32_at(row, 60))?;
        // Version 6 tail: the authority-bound flag. Version 5 rows carry no
        // such fact.
        let bound = if self.version == 6 {
            let bound = flag_at(row, 64).ok_or(ImageError::MethodReserved { index })?;
            if row[65..72] != [0; 7] {
                return Err(ImageError::MethodReserved { index });
            }
            bound
        } else {
            // Version 5 rows end at the file cell: no bound fact exists.
            false
        };
        Ok(MethodRow {
            owner,
            exported,
            pointer_receiver,
            promoted,
            name,
            type_root,
            receiver,
            receiver_type_params,
            origin,
            span,
            bound,
            file,
        })
    }

    /// Iterates every method row in producer order.
    pub fn methods(self) -> impl Iterator<Item = Result<MethodRow<'image>, ImageError>> {
        (0..self.method_count).map(move |index| self.method(index))
    }

    /// Borrows one validated type-parameter row.
    pub fn type_parameter(self, index: usize) -> Result<TypeParameterRow<'image>, ImageError> {
        if index >= self.type_parameter_count {
            return Err(ImageError::RowBounds {
                plane: "type parameter",
                index,
                count: self.type_parameter_count,
            });
        }
        let row = self.plane_row(self.type_parameters_offset, index, TYPE_PARAMETER_BYTES);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.declaration_count) {
            return Err(ImageError::TypeParameterOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let name = self.atom("type parameter", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "type parameter",
                index,
            });
        }
        let constraint = optional_row(u32_at(row, 12));
        if let Some(root) = out_of_bounds_root(constraint, self.type_count) {
            return Err(ImageError::TypeParameterConstraint {
                index,
                root,
                type_count: self.type_count,
            });
        }
        Ok(TypeParameterRow {
            owner,
            name,
            constraint,
        })
    }

    /// Borrows one validated member row.
    pub fn member(self, index: usize) -> Result<MemberRow<'image>, ImageError> {
        if index >= self.member_count {
            return Err(ImageError::RowBounds {
                plane: "member",
                index,
                count: self.member_count,
            });
        }
        let row = self.plane_row(self.members_offset, index, MEMBER_BYTES);
        let kind = match row[4] {
            0 => MemberKind::Field,
            1 => MemberKind::Method,
            found => return Err(ImageError::MemberKind { index, found }),
        };
        let embedded = flag_at(row, 5).ok_or(ImageError::MemberFlag {
            index,
            cell: "embedded",
            found: row[5],
        })?;
        if embedded && kind != MemberKind::Field {
            return Err(ImageError::MemberEmbedded { index });
        }
        let exported = flag_at(row, 6).ok_or(ImageError::MemberFlag {
            index,
            cell: "exported",
            found: row[6],
        })?;
        if row[7] != 0 || row[36..40] != [0; 4] {
            return Err(ImageError::MemberReserved { index });
        }
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::MemberOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let name = self.atom("member", index, u32_at(row, 8), u32_at(row, 12))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "member",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 16));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MemberTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let tag = self.atom("member", index, u32_at(row, 20), u32_at(row, 24))?;
        let package = self.atom("member", index, u32_at(row, 28), u32_at(row, 32))?;
        Ok(MemberRow {
            owner,
            kind,
            embedded,
            exported,
            name,
            type_root,
            tag,
            package,
        })
    }

    /// Borrows one validated documentation row.
    pub fn doc(self, index: usize) -> Result<DocRow<'image>, ImageError> {
        if index >= self.doc_count {
            return Err(ImageError::RowBounds {
                plane: "doc",
                index,
                count: self.doc_count,
            });
        }
        let row = self.plane_row(self.docs_offset, index, DOC_BYTES);
        let owner_kind = match row[0] {
            0 => DocOwner::Declaration,
            1 => DocOwner::Method,
            2 => DocOwner::Member,
            3 => DocOwner::Package,
            found => return Err(ImageError::DocOwnerKind { index, found }),
        };
        if row[1..4] != [0; 3] {
            return Err(ImageError::DocReserved { index });
        }
        let bound = match owner_kind {
            DocOwner::Declaration => self.declaration_count,
            DocOwner::Method => self.method_count,
            DocOwner::Member => self.member_count,
            DocOwner::Package => self.declaration_count,
        };
        let owner = u32_at(row, 4);
        if usize::try_from(owner).is_ok_and(|owner| owner >= bound) {
            return Err(ImageError::DocOwner {
                index,
                owner,
                bound,
            });
        }
        let text = self.atom("doc", index, u32_at(row, 8), u32_at(row, 12))?;
        if text.is_empty() {
            return Err(ImageError::EmptyDoc { index });
        }
        Ok(DocRow {
            owner_kind,
            owner,
            text,
        })
    }

    /// Borrows one validated reference row with its resolved, owner-relative
    /// span.
    pub fn reference(self, index: usize) -> Result<ReferenceRow<'image>, ImageError> {
        if index >= self.reference_count {
            return Err(ImageError::RowBounds {
                plane: "reference",
                index,
                count: self.reference_count,
            });
        }
        let row = self.plane_row(self.references_offset, index, self.reference_bytes);
        let owner = u32_at(row, 0);
        let Ok(owner_index) = usize::try_from(owner) else {
            return Err(ImageError::ReferenceOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        };
        if owner_index >= self.declaration_count {
            return Err(ImageError::ReferenceOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let target = self.atom("reference", index, u32_at(row, 4), u32_at(row, 8))?;
        if target.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "reference target",
                index,
            });
        }
        let target_package = self.atom("reference", index, u32_at(row, 12), u32_at(row, 16))?;
        let start = u32_at(row, 20);
        let end = u32_at(row, 24);
        if start > end {
            return Err(ImageError::ReferenceSpan { index, start, end });
        }
        let file = self.atom("reference", index, u32_at(row, 28), u32_at(row, 32))?;
        let receiver = self.atom("reference", index, u32_at(row, 36), u32_at(row, 40))?;
        // Version 6: the closed use kind, the used object's closed class,
        // and the target receiver type-name atom. Version 5 rows carry a
        // four-byte reserved tail and only ever record free-function calls.
        let (use_kind, target_class, recv_type) = if self.version == 6 {
            let use_kind =
                ReferenceUseKind::decode(row[44]).ok_or(ImageError::ReferenceReserved { index })?;
            let target_class = ReferenceTargetClass::decode(row[45])
                .ok_or(ImageError::ReferenceReserved { index })?;
            if row[46..48] != [0; 2] {
                return Err(ImageError::ReferenceReserved { index });
            }
            let recv_type = self.atom("reference", index, u32_at(row, 48), u32_at(row, 52))?;
            (use_kind, target_class, recv_type)
        } else {
            if row[44..48] != [0; 4] {
                return Err(ImageError::ReferenceReserved { index });
            }
            (
                ReferenceUseKind::Call,
                ReferenceTargetClass::Func,
                &self.bytes[0..0],
            )
        };
        let (owner_is_declaration, owner_row, owner_span, owner_file) = if receiver.is_empty() {
            let declaration = self.declaration(owner_index)?;
            (true, owner, declaration.span, declaration.file)
        } else {
            match self.method_containing(owner_index, start, end, file) {
                Some(method_index) => {
                    let method = self.method(method_index)?;
                    (false, method_index as u32, method.span, method.file)
                }
                None => {
                    return Err(ImageError::ReferenceOwnerUnresolved {
                        index,
                        owner_bytes: receiver.len(),
                        function_bytes: target.len(),
                    });
                }
            }
        };
        let Some((owner_start, owner_end)) = owner_span else {
            return Err(ImageError::ReferenceOwnerSpan { index });
        };
        if file != owner_file {
            return Err(ImageError::ReferenceFile { index });
        }
        if start < owner_start || end > owner_end {
            return Err(ImageError::ReferenceContainment {
                index,
                start,
                end,
                owner_start,
                owner_end,
            });
        }
        Ok(ReferenceRow {
            owner,
            target,
            target_package,
            receiver,
            span: (start, end),
            file,
            owner_is_declaration,
            owner_row,
            relative: (start - owner_start, end - owner_start),
            use_kind,
            target_class,
            recv_type,
        })
    }

    /// Iterates every reference row in producer order.
    pub fn references(self) -> impl Iterator<Item = Result<ReferenceRow<'image>, ImageError>> {
        (0..self.reference_count).map(move |index| self.reference(index))
    }

    /// Borrows one validated build-constraint row.
    pub fn constraint(self, index: usize) -> Result<ConstraintRow<'image>, ImageError> {
        if index >= self.constraint_count {
            return Err(ImageError::RowBounds {
                plane: "build constraint",
                index,
                count: self.constraint_count,
            });
        }
        let row = self.plane_row(self.constraints_offset, index, CONSTRAINT_BYTES);
        let file = self.atom("build constraint", index, u32_at(row, 0), u32_at(row, 4))?;
        if file.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "build constraint",
                index,
            });
        }
        let constraint = self.atom("build constraint", index, u32_at(row, 8), u32_at(row, 12))?;
        if constraint.is_empty() {
            return Err(ImageError::EmptyConstraint { index });
        }
        let blob_length = u32_at(row, 20);
        let exported = self.atom("build constraint", index, u32_at(row, 16), blob_length)?;
        let exported_count = u32_at(row, 24);
        if parse_constraint_blob(exported, exported_count).is_none() {
            return Err(ImageError::ConstraintBlob {
                index,
                count: exported_count,
                blob_bytes: blob_length,
            });
        }
        Ok(ConstraintRow {
            file,
            constraint,
            exported,
            exported_count,
        })
    }

    /// Iterates every build-constraint row in producer order.
    pub fn constraints(self) -> impl Iterator<Item = Result<ConstraintRow<'image>, ImageError>> {
        (0..self.constraint_count).map(move |index| self.constraint(index))
    }

    /// Borrows one validated interface-satisfaction row.
    pub fn satisfaction(self, index: usize) -> Result<SatisfactionRow<'image>, ImageError> {
        if index >= self.satisfaction_count {
            return Err(ImageError::RowBounds {
                plane: "satisfaction",
                index,
                count: self.satisfaction_count,
            });
        }
        let row = self.plane_row(self.satisfactions_offset, index, SATISFACTION_BYTES);
        let subject = u32_at(row, 0);
        if usize::try_from(subject).is_ok_and(|subject| subject >= self.declaration_count) {
            return Err(ImageError::SatisfactionSubject {
                index,
                subject,
                declaration_count: self.declaration_count,
            });
        }
        let target = self.atom("satisfaction", index, u32_at(row, 4), u32_at(row, 8))?;
        if target.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "satisfaction target",
                index,
            });
        }
        let target_package = self.atom("satisfaction", index, u32_at(row, 12), u32_at(row, 16))?;
        Ok(SatisfactionRow {
            subject,
            target,
            target_package,
        })
    }

    /// Iterates every satisfaction row in producer order.
    pub fn satisfactions(
        self,
    ) -> impl Iterator<Item = Result<SatisfactionRow<'image>, ImageError>> {
        (0..self.satisfaction_count).map(move |index| self.satisfaction(index))
    }

    /// Borrows the validated module-metadata row, when the oracle resolved a
    /// module.
    pub fn module(self) -> Result<Option<ModuleRow<'image>>, ImageError> {
        if self.module_count == 0 {
            return Ok(None);
        }
        let row = self.plane_row(self.module_offset, 0, MODULE_BYTES);
        Ok(Some(ModuleRow {
            path: self.atom("module", 0, u32_at(row, 0), u32_at(row, 4))?,
            directory: self.atom("module", 0, u32_at(row, 8), u32_at(row, 12))?,
            go_version: self.atom("module", 0, u32_at(row, 16), u32_at(row, 20))?,
            version: self.atom("module", 0, u32_at(row, 24), u32_at(row, 28))?,
        }))
    }

    /// Number of validated package rows.
    #[must_use]
    pub const fn package_count(self) -> usize {
        self.package_count
    }

    /// Number of validated module rows (zero or one).
    #[must_use]
    pub const fn module_count(self) -> usize {
        self.module_count
    }

    /// Borrows one validated package row.
    pub fn package(self, index: usize) -> Result<PackageRow<'image>, ImageError> {
        if index >= self.package_count {
            return Err(ImageError::RowBounds {
                plane: "package",
                index,
                count: self.package_count,
            });
        }
        let row = self.plane_row(self.packages_offset, index, PACKAGE_BYTES);
        let import_path = self.atom("package", index, u32_at(row, 0), u32_at(row, 4))?;
        if import_path.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "package",
                index,
            });
        }
        let name = self.atom("package", index, u32_at(row, 8), u32_at(row, 12))?;
        let blob_bytes = u32_at(row, 20);
        let files = self.atom("package", index, u32_at(row, 16), blob_bytes)?;
        let file_count = u32_at(row, 24);
        if split_nul(files, file_count).is_none() {
            return Err(ImageError::PackageFiles {
                index,
                count: file_count,
                blob_bytes,
            });
        }
        Ok(PackageRow {
            import_path,
            name,
            files,
            file_count,
        })
    }

    /// Iterates every package row in producer order.
    pub fn packages(self) -> impl Iterator<Item = Result<PackageRow<'image>, ImageError>> {
        (0..self.package_count).map(move |index| self.package(index))
    }

    /// Number of validated signature-parameter rows.
    #[must_use]
    pub const fn signature_parameter_count(self) -> usize {
        self.signature_parameter_count
    }

    /// Borrows one validated signature-parameter row: the exact source name
    /// and source position of one parameter or result of one func type row.
    /// The name may be empty — Go permits unnamed parameters and results —
    /// and a file-less position must be fully absent.
    pub fn signature_parameter(
        self,
        index: usize,
    ) -> Result<SignatureParameterRow<'image>, ImageError> {
        if index >= self.signature_parameter_count {
            return Err(ImageError::RowBounds {
                plane: "signature parameter",
                index,
                count: self.signature_parameter_count,
            });
        }
        let row = self.plane_row(
            self.signature_parameters_offset,
            index,
            SIGNATURE_PARAMETER_BYTES,
        );
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::SignatureParameterOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let file = self.atom(
            "signature parameter",
            index,
            u32_at(row, 16),
            u32_at(row, 20),
        )?;
        let offset = u32_at(row, 24);
        if file.is_empty() != (offset == NONE) {
            return Err(ImageError::SignatureParameterPosition { index });
        }
        Ok(SignatureParameterRow {
            owner,
            ordinal: u32_at(row, 4),
            name: self.atom(
                "signature parameter",
                index,
                u32_at(row, 8),
                u32_at(row, 12),
            )?,
            file,
            offset,
        })
    }

    /// Iterates every signature-parameter row in producer order.
    pub fn signature_parameters(
        self,
    ) -> impl Iterator<Item = Result<SignatureParameterRow<'image>, ImageError>> {
        (0..self.signature_parameter_count).map(move |index| self.signature_parameter(index))
    }

    /// Number of validated method-set rows.
    #[must_use]
    pub const fn method_set_count(self) -> usize {
        self.method_set_count
    }

    /// Borrows one validated method-set row: one method of an interface
    /// type row's complete post-embedding method set.
    pub fn method_set(self, index: usize) -> Result<MethodSetRow<'image>, ImageError> {
        if index >= self.method_set_count {
            return Err(ImageError::RowBounds {
                plane: "method set",
                index,
                count: self.method_set_count,
            });
        }
        let row = self.plane_row(self.method_sets_offset, index, METHOD_SET_BYTES);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::MethodSetOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let name = self.atom("method set", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "method set",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 12));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MethodSetTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        Ok(MethodSetRow {
            owner,
            name,
            type_root,
            package: self.atom("method set", index, u32_at(row, 16), u32_at(row, 20))?,
        })
    }

    /// Borrows one complete interface method-set row.
    pub fn interface_method_set(self, index: usize) -> Result<MethodSetRow<'image>, ImageError> {
        self.method_set(index)
    }

    /// Number of complete interface method-set rows.
    #[must_use]
    pub const fn interface_method_set_count(self) -> usize {
        self.method_set_count
    }

    /// Iterates every method-set row in producer order.
    pub fn method_sets(self) -> impl Iterator<Item = Result<MethodSetRow<'image>, ImageError>> {
        (0..self.method_set_count).map(move |index| self.method_set(index))
    }

    /// Resolves the method row declared on the receiver type `receiver` with
    /// the method name `name`, if any.
    #[must_use]
    pub fn method_row_of(self, name: &[u8], receiver: &[u8]) -> Option<usize> {
        for index in 0..self.method_count {
            let Ok(row) = self.method(index) else {
                continue;
            };
            let Ok(owner) = self.declaration(usize::try_from(row.owner).ok()?) else {
                continue;
            };
            if row.name == name && owner.name == receiver {
                return Some(index);
            }
        }
        None
    }

    /// Resolves the calling method row of one reference: the method owned by
    /// `owner` whose file and span contain the call site, if any.
    fn method_containing(self, owner: usize, start: u32, end: u32, file: &[u8]) -> Option<usize> {
        for index in 0..self.method_count {
            let Ok(row) = self.method(index) else {
                continue;
            };
            if usize::try_from(row.owner) != Ok(owner) || row.file != file {
                continue;
            }
            if let Some((row_start, row_end)) = row.span
                && start >= row_start
                && end <= row_end
            {
                return Some(index);
            }
        }
        None
    }

    fn plane_row(self, plane_offset: usize, index: usize, width: usize) -> &'image [u8] {
        let start = plane_offset + index * width;
        &self.bytes[start..start + width]
    }

    fn atom(
        self,
        plane: &'static str,
        index: usize,
        offset: u32,
        length: u32,
    ) -> Result<&'image [u8], ImageError> {
        let range_fault = || ImageError::AtomRange {
            plane,
            index,
            offset: usize::MAX,
            length: usize::MAX,
            atom_bytes: self.atom_bytes,
        };
        let offset = usize::try_from(offset).map_err(|_| range_fault())?;
        let length = usize::try_from(length).map_err(|_| range_fault())?;
        let Some(end) = offset.checked_add(length) else {
            return Err(range_fault());
        };
        if end > self.atom_bytes {
            return Err(ImageError::AtomRange {
                plane,
                index,
                offset,
                length,
                atom_bytes: self.atom_bytes,
            });
        }
        if length == 0 {
            return Ok(&self.bytes[0..0]);
        }
        let start = self.atom_offset + offset;
        let bytes = &self.bytes[start..start + length];
        if str::from_utf8(bytes).is_err() {
            return Err(ImageError::AtomUtf8 { plane, index });
        }
        Ok(bytes)
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(self.digest_domain);
        digest.update(&self.bytes[..52]);
        digest.update(&self.bytes[84..HEADER_BYTES]);
        digest.update(&self.bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice() != &self.bytes[52..84] {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.declaration_count {
            self.declaration(index)?;
        }
        Ok(())
    }

    fn validate_module(self) -> Result<(), ImageError> {
        if let Some(module) = self.module()?
            && module.path.is_empty()
        {
            return Err(ImageError::ModulePath);
        }
        Ok(())
    }

    /// Package rows must be canonically ordered by import path, and every
    /// declaration must name one of the package rows' import paths.
    fn validate_packages(self) -> Result<(), ImageError> {
        let mut previous_path: Option<&'image [u8]> = None;
        for index in 0..self.package_count {
            let row = self.package(index)?;
            if previous_path.is_some_and(|previous| row.import_path <= previous) {
                return Err(ImageError::PackageSort { index });
            }
            previous_path = Some(row.import_path);
        }
        for index in 0..self.declaration_count {
            let declared = self.declaration(index)?;
            let mut bound = false;
            for package_index in 0..self.package_count {
                let row = self.package(package_index)?;
                if declared.package == row.import_path {
                    bound = true;
                    break;
                }
            }
            if !bound {
                return Err(ImageError::DeclarationPackage {
                    index,
                    package_count: self.package_count,
                });
            }
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        let mut expected_child = 0_usize;
        let mut signature_parameter_cursor = 0_usize;
        for index in 0..self.type_count {
            let row = self.type_row(index)?;
            let name_required = matches!(
                row.kind,
                TypeRowKind::Basic
                    | TypeRowKind::Named
                    | TypeRowKind::Alias
                    | TypeRowKind::TypeParam
            );
            if name_required && row.name.is_empty() {
                return Err(ImageError::TypeNameRequired {
                    index,
                    kind: row.kind,
                });
            }
            let name_forbidden = matches!(
                row.kind,
                TypeRowKind::Pointer
                    | TypeRowKind::Slice
                    | TypeRowKind::Array
                    | TypeRowKind::Map
                    | TypeRowKind::Chan
                    | TypeRowKind::Func
                    | TypeRowKind::Struct
                    | TypeRowKind::Interface
                    | TypeRowKind::Union
                    | TypeRowKind::Tuple
            );
            if name_forbidden && !row.name.is_empty() {
                return Err(ImageError::TypeNameForbidden {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind != TypeRowKind::Chan && row.dir != ChanDir::Both {
                return Err(ImageError::TypeDirectionCell {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind != TypeRowKind::Func && row.variadic {
                return Err(ImageError::TypeVariadicCell {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind == TypeRowKind::Array && row.length < 0 {
                return Err(ImageError::ArrayLength {
                    index,
                    length: row.length,
                });
            }
            let (min_children, max_children) = match row.kind {
                TypeRowKind::Pointer
                | TypeRowKind::Slice
                | TypeRowKind::Array
                | TypeRowKind::Chan => (1, Some(1)),
                TypeRowKind::Map => (2, Some(2)),
                TypeRowKind::Basic
                | TypeRowKind::TypeParam
                | TypeRowKind::Struct
                | TypeRowKind::Invalid => (0, Some(0)),
                TypeRowKind::Named
                | TypeRowKind::Alias
                | TypeRowKind::Func
                | TypeRowKind::Interface
                | TypeRowKind::Union
                | TypeRowKind::Tuple => (0, None),
            };
            let child_start =
                usize::try_from(row.children.0).map_err(|_| ImageError::TypeChildRange {
                    index,
                    start: usize::MAX,
                    count: usize::try_from(row.children.1).unwrap_or(usize::MAX),
                    child_count: self.child_count,
                })?;
            let child_count =
                usize::try_from(row.children.1).map_err(|_| ImageError::TypeChildRange {
                    index,
                    start: child_start,
                    count: usize::MAX,
                    child_count: self.child_count,
                })?;
            let param_count_law = match row.kind {
                TypeRowKind::Func => {
                    usize::try_from(row.param_count).is_ok_and(|count| count <= child_count)
                }
                _ => row.param_count == 0,
            };
            if !param_count_law {
                return Err(ImageError::TypeParamCount {
                    index,
                    kind: row.kind,
                    param_count: row.param_count,
                    child_count: row.children.1,
                });
            }
            let over_max = max_children.is_some_and(|max| child_count > max);
            let run_end = child_start.checked_add(child_count);
            if child_count < min_children
                || over_max
                || child_start != expected_child
                || run_end.is_none_or(|end| end > self.child_count)
            {
                return Err(ImageError::TypeChildRange {
                    index,
                    start: child_start,
                    count: child_count,
                    child_count: self.child_count,
                });
            }
            for child in child_start..child_start + child_count {
                self.type_child(child)?;
            }
            expected_child = child_start + child_count;
            let member_start =
                usize::try_from(row.members.0).map_err(|_| ImageError::TypeMemberRange {
                    index,
                    start: usize::MAX,
                    count: usize::try_from(row.members.1).unwrap_or(usize::MAX),
                    member_count: self.member_count,
                })?;
            let member_count =
                usize::try_from(row.members.1).map_err(|_| ImageError::TypeMemberRange {
                    index,
                    start: member_start,
                    count: usize::MAX,
                    member_count: self.member_count,
                })?;
            let member_law = match row.kind {
                TypeRowKind::Struct | TypeRowKind::Interface => None,
                _ => Some(0),
            };
            let member_end = member_start.checked_add(member_count);
            if member_law.is_some_and(|max| member_count > max)
                || member_end.is_none_or(|end| end > self.member_count)
            {
                return Err(ImageError::TypeMemberRange {
                    index,
                    start: member_start,
                    count: member_count,
                    member_count: self.member_count,
                });
            }
            // Every func row owns exactly one signature-parameter row per
            // child: parameters first, then results, ordinals ascending.
            if row.kind == TypeRowKind::Func {
                for ordinal in 0..child_count {
                    let row_index = signature_parameter_cursor + ordinal;
                    let parameter = self.signature_parameter(row_index)?;
                    if usize::try_from(parameter.owner).unwrap_or(usize::MAX) != index {
                        return Err(ImageError::SignatureParameterOwnerRow {
                            index: row_index,
                            owner: parameter.owner,
                            expected: u32::try_from(index).unwrap_or(u32::MAX),
                        });
                    }
                    if parameter.ordinal != u32::try_from(ordinal).unwrap_or(u32::MAX) {
                        return Err(ImageError::SignatureParameterOrdinal {
                            index: row_index,
                            ordinal: parameter.ordinal,
                            expected: u32::try_from(ordinal).unwrap_or(u32::MAX),
                        });
                    }
                }
                signature_parameter_cursor += child_count;
            }
        }
        if expected_child != self.child_count {
            return Err(ImageError::TypeChildTiling {
                declared: expected_child,
                plane: self.child_count,
            });
        }
        if signature_parameter_cursor != self.signature_parameter_count {
            return Err(ImageError::SignatureParameterTiling {
                declared: signature_parameter_cursor,
                plane: self.signature_parameter_count,
            });
        }
        Ok(())
    }

    fn validate_methods(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.method_count {
            let row = self.method(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::MethodSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        Ok(())
    }

    fn validate_type_parameters(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.type_parameter_count {
            let row = self.type_parameter(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::TypeParameterSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        Ok(())
    }

    fn validate_members(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.member_count {
            let row = self.member(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::MemberSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        // Every owner's declared run must equal exactly its contiguous run
        // of same-owner rows, so the plane tiles without gaps or overlaps.
        let mut cursor = 0_usize;
        while cursor < self.member_count {
            let owner = self.member(cursor)?.owner;
            let mut end = cursor + 1;
            while end < self.member_count && self.member(end)?.owner == owner {
                end += 1;
            }
            let owner_index = usize::try_from(owner).map_err(|_| ImageError::MemberOwnerRange {
                owner,
                start: usize::MAX,
                count: 0,
                actual_start: cursor,
                actual_count: end - cursor,
            })?;
            let declared = self.type_row(owner_index)?.members;
            let declared_start = usize::try_from(declared.0).unwrap_or(usize::MAX);
            let declared_count = usize::try_from(declared.1).unwrap_or(usize::MAX);
            if declared_start != cursor || declared_count != end - cursor {
                return Err(ImageError::MemberOwnerRange {
                    owner,
                    start: declared_start,
                    count: declared_count,
                    actual_start: cursor,
                    actual_count: end - cursor,
                });
            }
            cursor = end;
        }
        Ok(())
    }

    /// Method-set rows must be canonically ordered by (owner, name) with
    /// owner-contiguous runs in type-row order, and every owner must be an
    /// interface type row.
    fn validate_method_sets(self) -> Result<(), ImageError> {
        let mut previous: Option<(u32, &'image [u8])> = None;
        for index in 0..self.method_set_count {
            let row = self.method_set(index)?;
            if previous.is_some_and(|(previous_owner, previous_name)| {
                row.owner < previous_owner
                    || (row.owner == previous_owner && row.name <= previous_name)
            }) {
                return Err(ImageError::MethodSetSort { index });
            }
            let owner = usize::try_from(row.owner).unwrap_or(usize::MAX);
            if owner < self.type_count && self.type_row(owner)?.kind != TypeRowKind::Interface {
                return Err(ImageError::MethodSetOwnerKind {
                    index,
                    kind: self.type_row(owner)?.kind,
                });
            }
            previous = Some((row.owner, row.name));
        }
        Ok(())
    }

    fn validate_docs(self) -> Result<(), ImageError> {
        let mut previous = (0_u8, 0_u32);
        for index in 0..self.doc_count {
            let row = self.doc(index)?;
            let key = (row.owner_kind as u8, row.owner);
            if index > 0 && key < previous {
                return Err(ImageError::DocSort {
                    index,
                    owner_kind: key.0,
                    owner: key.1,
                    previous_kind: previous.0,
                    previous_owner: previous.1,
                });
            }
            previous = key;
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        let mut previous: Option<(&'image [u8], u32)> = None;
        for index in 0..self.reference_count {
            let row = self.reference(index)?;
            if previous.is_some_and(|(previous_file, previous_start)| {
                row.file < previous_file
                    || (row.file == previous_file && row.span.0 < previous_start)
            }) {
                return Err(ImageError::ReferenceSort { index });
            }
            previous = Some((row.file, row.span.0));
        }
        Ok(())
    }

    fn validate_constraints(self) -> Result<(), ImageError> {
        let mut previous_file: Option<&'image [u8]> = None;
        for index in 0..self.constraint_count {
            let row = self.constraint(index)?;
            if previous_file.is_some_and(|previous| row.file < previous) {
                return Err(ImageError::ConstraintSort { index });
            }
            previous_file = Some(row.file);
        }
        Ok(())
    }

    /// Satisfaction rows must be canonically ordered by subject, and every
    /// subject must be a named-type declaration: the oracle only records
    /// method-set satisfaction for `type` declarations.
    fn validate_satisfactions(self) -> Result<(), ImageError> {
        let mut previous_subject = 0_u32;
        for index in 0..self.satisfaction_count {
            let row = self.satisfaction(index)?;
            if index > 0 && row.subject < previous_subject {
                return Err(ImageError::SatisfactionSort {
                    index,
                    subject: row.subject,
                    previous: previous_subject,
                });
            }
            let declaration = self.declaration(usize::try_from(row.subject).map_err(|_| {
                ImageError::SatisfactionSubject {
                    index,
                    subject: row.subject,
                    declaration_count: self.declaration_count,
                }
            })?)?;
            if declaration.kind != DeclarationKind::Type {
                return Err(ImageError::SatisfactionSubjectKind {
                    index,
                    kind: declaration.kind,
                });
            }
            previous_subject = row.subject;
        }
        Ok(())
    }
}

/// Child flag bit marking a tilde (`~T`) union term.
pub const CHILD_TILDE_FLAG: u32 = 1;

/// Exact image rejection returned before a Go fact is admitted.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ImageError {
    /// The fixed binary envelope is invalid.
    #[error("invalid Go authority image header: {0}")]
    Header(#[from] HeaderError),
    /// The image checksum differs from its fixed header and body.
    #[error("Go authority image checksum does not match")]
    Digest,
    /// A queried row index lies outside its validated plane.
    #[error("Go authority {plane} row {index} lies outside {count}")]
    RowBounds {
        /// Queried plane name.
        plane: &'static str,
        /// Queried row index.
        index: usize,
        /// Validated plane row count.
        count: usize,
    },
    /// A declaration row has an unrecognized kind tag.
    #[error("Go authority declaration {index} has unknown kind tag {found}")]
    DeclarationKind { index: usize, found: u8 },
    /// A declaration row has an invalid closed exported flag.
    #[error("Go authority declaration {index} has invalid exported flag {found}")]
    ExportedFlag { index: usize, found: u8 },
    /// A declaration row has an invalid closed iota flag.
    #[error("Go authority declaration {index} has invalid iota flag {found}")]
    DeclarationIota { index: usize, found: u8 },
    /// A declaration row claims non-zero reserved bits.
    #[error("Go authority declaration {index} has non-zero reserved bits")]
    DeclarationReserved { index: usize },
    /// A type row claims non-zero reserved bits.
    #[error("Go authority type row {index} has non-zero reserved bits")]
    TypeReserved { index: usize },
    /// A method row claims non-zero reserved bits.
    #[error("Go authority method {index} has non-zero reserved bits")]
    MethodReserved { index: usize },
    /// A member row claims non-zero reserved bits.
    #[error("Go authority member {index} has non-zero reserved bits")]
    MemberReserved { index: usize },
    /// A doc row claims non-zero reserved bits.
    #[error("Go authority doc {index} has non-zero reserved bits")]
    DocReserved { index: usize },
    /// A reference row claims non-zero reserved bits.
    #[error("Go authority reference {index} has non-zero reserved bits")]
    ReferenceReserved { index: usize },
    /// A declaration type-root cell names a row outside the type plane.
    #[error(
        "Go authority declaration {index} names type root {root} outside {type_count} type rows"
    )]
    DeclarationTypeRoot {
        index: usize,
        root: u32,
        type_count: usize,
    },
    /// A declaration or method span is inverted or half-present.
    #[error("Go authority row {index} has malformed span {start}..{end}")]
    DeclarationSpan { index: usize, start: u32, end: u32 },
    /// A type row has an unrecognized kind tag.
    #[error("Go authority type row {index} has unknown kind tag {found}")]
    TypeKind { index: usize, found: u8 },
    /// A type row name cell is required but empty.
    #[error("Go authority type row {index} of kind {kind:?} requires a name")]
    TypeNameRequired { index: usize, kind: TypeRowKind },
    /// A type row name cell carries bytes where the kind forbids them.
    #[error("Go authority type row {index} of kind {kind:?} forbids a name")]
    TypeNameForbidden { index: usize, kind: TypeRowKind },
    /// A type row direction cell is outside the closed set.
    #[error("Go authority type row {index} has unknown channel direction {found}")]
    TypeDirection { index: usize, found: u8 },
    /// A non-chan type row carries a channel direction.
    #[error("Go authority type row {index} of kind {kind:?} carries a channel direction")]
    TypeDirectionCell { index: usize, kind: TypeRowKind },
    /// A type row variadic cell is outside the closed set.
    #[error("Go authority type row {index} has unknown variadic flag {found}")]
    TypeVariadicFlag { index: usize, found: u8 },
    /// A non-func type row carries the variadic flag.
    #[error("Go authority type row {index} of kind {kind:?} carries the variadic flag")]
    TypeVariadicCell { index: usize, kind: TypeRowKind },
    /// An array row declares a negative length.
    #[error("Go authority array row {index} declares negative length {length}")]
    ArrayLength { index: usize, length: i64 },
    /// A func row's parameter count disagrees with its child run, or a
    /// non-func row carries a parameter count.
    #[error(
        "Go authority type row {index} of kind {kind:?} declares {param_count} parameters over {child_count} children"
    )]
    TypeParamCount {
        index: usize,
        kind: TypeRowKind,
        param_count: u32,
        child_count: u32,
    },
    /// A type row's child run is out of bounds, misordered, or off-law.
    #[error(
        "Go authority type row {index} child range {start}..{count} violates the child plane of {child_count}"
    )]
    TypeChildRange {
        index: usize,
        start: usize,
        count: usize,
        child_count: usize,
    },
    /// A pooled type child names a row outside the type plane.
    #[error("Go authority type child {index} names type row {target} outside {type_count}")]
    TypeChildTarget {
        index: usize,
        target: u32,
        type_count: usize,
    },
    /// A pooled type child carries undefined flag bits.
    #[error("Go authority type child {index} carries undefined flags {flags:#x}")]
    TypeChildFlags { index: usize, flags: u32 },
    /// The pooled child plane does not exactly tile every type row's run.
    #[error("Go authority type child plane holds {plane} children but rows declare {declared}")]
    TypeChildTiling { declared: usize, plane: usize },
    /// A type row's member run is out of bounds or off-law.
    #[error(
        "Go authority type row {index} member range {start}..{count} violates the member plane of {member_count}"
    )]
    TypeMemberRange {
        index: usize,
        start: usize,
        count: usize,
        member_count: usize,
    },
    /// A method row names an owner outside the declaration plane.
    #[error(
        "Go authority method {index} names owner {owner} outside {declaration_count} declarations"
    )]
    MethodOwner {
        index: usize,
        owner: u32,
        declaration_count: usize,
    },
    /// A method row flag cell is outside the closed set.
    #[error("Go authority method {index} has unknown {cell} flag {found}")]
    MethodFlag {
        index: usize,
        cell: &'static str,
        found: u8,
    },
    /// A method signature root names a row outside the type plane.
    #[error("Go authority method {index} names type root {root} outside {type_count}")]
    MethodTypeRoot {
        index: usize,
        root: u32,
        type_count: usize,
    },
    /// A method's receiver type-parameter blob disagrees with its count.
    #[error(
        "Go authority method {index} receiver type-parameter blob of {blob_bytes} bytes does not hold {count} names"
    )]
    MethodReceiverParams {
        index: usize,
        count: u32,
        blob_bytes: u32,
    },
    /// Method rows are not canonically ordered by owner.
    #[error("Go authority method {index} owner {owner} precedes earlier owner {previous}")]
    MethodSort {
        index: usize,
        owner: u32,
        previous: u32,
    },
    /// A type-parameter row names an owner outside the declaration plane.
    #[error(
        "Go authority type parameter {index} names owner {owner} outside {declaration_count} declarations"
    )]
    TypeParameterOwner {
        index: usize,
        owner: u32,
        declaration_count: usize,
    },
    /// A type-parameter constraint names a row outside the type plane.
    #[error(
        "Go authority type parameter {index} constraint names type row {root} outside {type_count}"
    )]
    TypeParameterConstraint {
        index: usize,
        root: u32,
        type_count: usize,
    },
    /// Type-parameter rows are not canonically ordered by owner.
    #[error("Go authority type parameter {index} owner {owner} precedes earlier owner {previous}")]
    TypeParameterSort {
        index: usize,
        owner: u32,
        previous: u32,
    },
    /// A member row has an unknown member kind.
    #[error("Go authority member {index} has unknown kind tag {found}")]
    MemberKind { index: usize, found: u8 },
    /// A member row flag cell is outside the closed set.
    #[error("Go authority member {index} has unknown {cell} flag {found}")]
    MemberFlag {
        index: usize,
        cell: &'static str,
        found: u8,
    },
    /// A non-field member carries the embedded flag.
    #[error("Go authority member {index} carries the embedded flag off a struct field")]
    MemberEmbedded { index: usize },
    /// A member row names an owner outside the type plane.
    #[error("Go authority member {index} names owner {owner} outside {type_count} type rows")]
    MemberOwner {
        index: usize,
        owner: u32,
        type_count: usize,
    },
    /// A member type root names a row outside the type plane.
    #[error("Go authority member {index} names type root {root} outside {type_count}")]
    MemberTypeRoot {
        index: usize,
        root: u32,
        type_count: usize,
    },
    /// Member rows are not canonically ordered by owner.
    #[error("Go authority member {index} owner {owner} precedes earlier owner {previous}")]
    MemberSort {
        index: usize,
        owner: u32,
        previous: u32,
    },
    /// A type row's declared member run disagrees with its contiguous rows.
    #[error(
        "Go authority type row {owner} declares members {start}..{count} but holds rows {actual_start}..{actual_count}"
    )]
    MemberOwnerRange {
        owner: u32,
        start: usize,
        count: usize,
        actual_start: usize,
        actual_count: usize,
    },
    /// A documentation row has an unknown owner-kind tag.
    #[error("Go authority doc {index} has unknown owner kind {found}")]
    DocOwnerKind { index: usize, found: u8 },
    /// A documentation row names an owner outside its lane.
    #[error("Go authority doc {index} names owner {owner} outside {bound}")]
    DocOwner {
        index: usize,
        owner: u32,
        bound: usize,
    },
    /// A documentation row carries empty text.
    #[error("Go authority doc {index} carries empty text")]
    EmptyDoc { index: usize },
    /// Documentation rows are not canonically ordered by owner key.
    #[error(
        "Go authority doc {index} owner ({owner_kind}, {owner}) precedes earlier ({previous_kind}, {previous_owner})"
    )]
    DocSort {
        index: usize,
        owner_kind: u8,
        owner: u32,
        previous_kind: u8,
        previous_owner: u32,
    },
    /// A reference row names an owner outside the declaration plane.
    #[error(
        "Go authority reference {index} names owner {owner} outside {declaration_count} declarations"
    )]
    ReferenceOwner {
        index: usize,
        owner: u32,
        declaration_count: usize,
    },
    /// A reference call span is inverted.
    #[error("Go authority reference {index} has inverted call span {start}..{end}")]
    ReferenceSpan { index: usize, start: u32, end: u32 },
    /// A method-owning reference names no such (receiver, method) row.
    #[error(
        "Go authority reference {index} names unresolved method of {function_bytes} bytes on receiver of {owner_bytes} bytes"
    )]
    ReferenceOwnerUnresolved {
        index: usize,
        owner_bytes: usize,
        function_bytes: usize,
    },
    /// The resolved reference owner has no span to measure against.
    #[error("Go authority reference {index} owner has no resolved span")]
    ReferenceOwnerSpan { index: usize },
    /// The reference call-site file differs from the owner's file.
    #[error("Go authority reference {index} names a file other than its owner's")]
    ReferenceFile { index: usize },
    /// The reference call span is not contained in the owner span.
    #[error(
        "Go authority reference {index} call span {start}..{end} escapes owner span {owner_start}..{owner_end}"
    )]
    ReferenceContainment {
        index: usize,
        start: u32,
        end: u32,
        owner_start: u32,
        owner_end: u32,
    },
    /// Reference rows are not canonically ordered by (file, start).
    #[error("Go authority reference {index} is out of canonical (file, start) order")]
    ReferenceSort { index: usize },
    /// A build-constraint row carries an empty constraint expression.
    #[error("Go authority constraint {index} carries an empty constraint")]
    EmptyConstraint { index: usize },
    /// A build-constraint blob disagrees with its declared record count.
    #[error(
        "Go authority constraint {index} exported blob of {blob_bytes} bytes does not hold {count} records"
    )]
    ConstraintBlob {
        index: usize,
        count: u32,
        blob_bytes: u32,
    },
    /// Build-constraint rows are not canonically ordered by file.
    #[error("Go authority constraint {index} is out of canonical file order")]
    ConstraintSort { index: usize },
    /// A satisfaction row names a subject outside the declaration plane.
    #[error(
        "Go authority satisfaction {index} names subject {subject} outside {declaration_count} declarations"
    )]
    SatisfactionSubject {
        index: usize,
        subject: u32,
        declaration_count: usize,
    },
    /// Satisfaction rows are not canonically ordered by subject.
    #[error(
        "Go authority satisfaction {index} subject {subject} precedes earlier subject {previous}"
    )]
    SatisfactionSort {
        index: usize,
        subject: u32,
        previous: u32,
    },
    /// A satisfaction row's subject is not a named-type declaration.
    #[error(
        "Go authority satisfaction {index} names a subject of kind {kind:?}; only named types carry satisfaction edges"
    )]
    SatisfactionSubjectKind { index: usize, kind: DeclarationKind },
    /// A present module row has no module path.
    #[error("Go authority module row has an empty path")]
    ModulePath,
    /// A package row's file blob disagrees with its declared file count.
    #[error(
        "Go authority package {index} file blob of {blob_bytes} bytes does not hold {count} names"
    )]
    PackageFiles {
        index: usize,
        count: u32,
        blob_bytes: u32,
    },
    /// Package rows are not canonically ordered by import path.
    #[error("Go authority package {index} is out of canonical import-path order")]
    PackageSort { index: usize },
    /// A declaration names a package outside the package rows.
    #[error(
        "Go authority declaration {index} names a package outside the {package_count} package rows"
    )]
    DeclarationPackage { index: usize, package_count: usize },
    /// A signature-parameter row's position is half-present: a file without
    /// an offset or an offset without a file.
    #[error("Go authority signature parameter {index} has a malformed source position")]
    SignatureParameterPosition { index: usize },
    /// A method-set row names an owner outside the type plane.
    #[error("Go authority method set {index} names owner {owner} outside {type_count} type rows")]
    MethodSetOwner {
        index: usize,
        owner: u32,
        type_count: usize,
    },
    /// A method-set row names a non-interface type row as its owner.
    #[error(
        "Go authority method set {index} names a type row of kind {kind:?}; only interfaces carry method sets"
    )]
    MethodSetOwnerKind { index: usize, kind: TypeRowKind },
    /// A method-set signature root names a row outside the type plane.
    #[error("Go authority method set {index} names type root {root} outside {type_count}")]
    MethodSetTypeRoot {
        index: usize,
        root: u32,
        type_count: usize,
    },
    /// Method-set rows are not canonically ordered by (owner, name).
    #[error("Go authority method set {index} is out of canonical (owner, name) order")]
    MethodSetSort { index: usize },
    /// A signature-parameter row names an owner outside the type plane.
    #[error(
        "Go authority signature parameter {index} names owner {owner} outside {type_count} type rows"
    )]
    SignatureParameterOwner {
        index: usize,
        owner: u32,
        type_count: usize,
    },
    /// A signature-parameter row names an owner other than the func row that
    /// owns its run.
    #[error(
        "Go authority signature parameter {index} names owner {owner}; the run belongs to type row {expected}"
    )]
    SignatureParameterOwnerRow {
        index: usize,
        owner: u32,
        expected: u32,
    },
    /// A signature-parameter row's ordinal disagrees with its run position.
    #[error(
        "Go authority signature parameter {index} carries ordinal {ordinal}; the run expects {expected}"
    )]
    SignatureParameterOrdinal {
        index: usize,
        ordinal: u32,
        expected: u32,
    },
    /// The signature-parameter plane does not exactly tile every func row's
    /// child run.
    #[error(
        "Go authority signature-parameter plane holds {plane} rows but func rows declare {declared}"
    )]
    SignatureParameterTiling { declared: usize, plane: usize },
    /// A row requires a non-empty name and carries none.
    #[error("Go authority {plane} row {index} carries an empty name")]
    EmptyName { plane: &'static str, index: usize },
    /// An atom range lies outside the atom plane.
    #[error(
        "Go authority {plane} row {index} atom range {offset}..{length} exceeds atom bytes {atom_bytes}"
    )]
    AtomRange {
        plane: &'static str,
        index: usize,
        offset: usize,
        length: usize,
        atom_bytes: usize,
    },
    /// An atom range is not valid UTF-8.
    #[error("Go authority {plane} row {index} atom bytes are not UTF-8")]
    AtomUtf8 { plane: &'static str, index: usize },
}

/// Exact fixed-header violation returned by [`GoImage::open`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HeaderError {
    /// Fewer than the complete fixed header bytes were supplied.
    #[error("truncated header: found {actual} bytes, need at least {HEADER_BYTES}")]
    Truncated { actual: usize },
    /// The four-byte image magic differs from `NGAI`.
    #[error("unexpected magic {found:?}")]
    Magic { found: [u8; 4] },
    /// The image version is not supported by this reader.
    #[error("unsupported image version {found}")]
    Version { found: u16 },
    /// The fixed header length does not match this image version.
    #[error("unexpected fixed header length {found}")]
    Length { found: usize },
    /// The body dimensions do not equal the supplied image body.
    #[error("declared body length {declared} differs from actual {actual}")]
    BodyLength { declared: usize, actual: usize },
    /// Reserved header bytes are not all zero.
    #[error("reserved header bytes are non-zero")]
    Reserved,
    /// The module plane has more than its permitted optional row.
    #[error("module plane holds {found} rows; at most one is permitted")]
    ModuleCount { found: usize },
}

/// Splits one NUL-separated name blob into exactly `count` non-empty parts.
#[must_use]
pub fn split_nul(blob: &[u8], count: u32) -> Option<Vec<&[u8]>> {
    let count = usize::try_from(count).ok()?;
    if count == 0 {
        return if blob.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    }
    let mut parts = Vec::new();
    let mut start = 0_usize;
    for (position, byte) in blob.iter().enumerate() {
        if *byte == 0 {
            if position == start {
                return None;
            }
            parts.push(&blob[start..position]);
            start = position + 1;
        }
    }
    if start != blob.len() {
        return None;
    }
    (parts.len() == count).then_some(parts)
}

/// Parses one build-constraint exported blob into its exact records:
/// `count` records of `(kind byte, name bytes, 0x00)`.
#[must_use]
pub fn parse_constraint_blob(blob: &[u8], count: u32) -> Option<Vec<ConstrainedDecl<'_>>> {
    let count = usize::try_from(count).ok()?;
    let mut out = Vec::new();
    let mut cursor = 0_usize;
    for _ in 0..count {
        let kind = DeclarationKind::decode(*blob.get(cursor)?)?;
        cursor += 1;
        let name_start = cursor;
        while cursor < blob.len() && blob[cursor] != 0 {
            cursor += 1;
        }
        if cursor >= blob.len() || cursor == name_start {
            return None;
        }
        out.push(ConstrainedDecl {
            kind,
            name: &blob[name_start..cursor],
        });
        cursor += 1;
    }
    (cursor == blob.len()).then_some(out)
}

/// Decodes one closed 0/1 flag cell.
const fn flag_at(row: &[u8], offset: usize) -> Option<bool> {
    match row[offset] {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Maps the optional-row sentinel to `None`.
const fn optional_row(raw: u32) -> Option<u32> {
    if raw == NONE { None } else { Some(raw) }
}

/// Whether one optional type-row coordinate is present and out of bounds.
fn optional_out_of_bounds(raw: Option<u32>, type_count: usize) -> bool {
    raw.is_some_and(|root| usize::try_from(root).is_ok_and(|root| root >= type_count))
}

/// The raw root cell when one optional type-row coordinate is present and
/// out of bounds, for exact error operands.
fn out_of_bounds_root(raw: Option<u32>, type_count: usize) -> Option<u32> {
    raw.filter(|root| optional_out_of_bounds(Some(*root), type_count))
}

/// Validates one span pair: both bounds share presence and the half-open
/// range is ordered.
fn span_at(
    row: &[u8],
    start_offset: usize,
    end_offset: usize,
    index: usize,
) -> Result<Option<(u32, u32)>, ImageError> {
    let start = u32_at(row, start_offset);
    let end = u32_at(row, end_offset);
    match (optional_row(start), optional_row(end)) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) if start <= end => Ok(Some((start, end))),
        (start, end) => Err(ImageError::DeclarationSpan {
            index,
            start: start.unwrap_or(NONE),
            end: end.unwrap_or(NONE),
        }),
    }
}

/// Reads one plane-count header cell.
fn plane_count(bytes: &[u8], offset: usize, actual_body: usize) -> Result<usize, ImageError> {
    let raw = u32_at(bytes, offset);
    let count = usize::try_from(raw).map_err(|_| {
        ImageError::Header(HeaderError::BodyLength {
            declared: usize::MAX,
            actual: actual_body,
        })
    })?;
    Ok(count)
}

const fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

const fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod unresolved_cgo_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    const DIGEST_DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v6\x00";
    /// Atoms: import path, package name, files blob, declaration name, then
    /// the two unresolved-cgo spellings.
    const ATOMS: &[u8] =
        b"example.com/cgo\0cgo\0main.go\0Conn\0C.sqlite3\0example.com/cgo.Conn";

    fn cells(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn reseal(image: &mut [u8]) {
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&image[..52]);
        digest.update(&image[84..HEADER_BYTES]);
        digest.update(&image[HEADER_BYTES..]);
        image[52..84].copy_from_slice(digest.finalize().as_slice());
    }

    fn package_row() -> Vec<u8> {
        let row = cells(&[0, 15, 16, 3, 20, 8, 1]);
        assert_eq!(row.len(), PACKAGE_BYTES);
        row
    }

    fn declaration_row() -> Vec<u8> {
        let mut row = vec![0_u8; DECLARATION_BYTES_V6];
        row[0] = 1;
        row[1] = 1;
        row[4..8].copy_from_slice(&28_u32.to_le_bytes());
        row[8..12].copy_from_slice(&4_u32.to_le_bytes());
        row[12..16].copy_from_slice(&0_u32.to_le_bytes());
        row[16..20].copy_from_slice(&15_u32.to_le_bytes());
        row[20..24].copy_from_slice(&NONE.to_le_bytes());
        row[24..28].copy_from_slice(&NONE.to_le_bytes());
        row[28..32].copy_from_slice(&NONE.to_le_bytes());
        row
    }

    fn write_header(image: &mut [u8], body_bytes: usize, unresolved_cgo_count: u32) {
        image[..4].copy_from_slice(b"NGAI");
        image[4..6].copy_from_slice(&6_u16.to_le_bytes());
        image[6..8].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        image[8..12].copy_from_slice(&1_u32.to_le_bytes());
        image[12..16].copy_from_slice(&(ATOMS.len() as u32).to_le_bytes());
        image[16..20].copy_from_slice(&(body_bytes as u32).to_le_bytes());
        image[116..120].copy_from_slice(&0_u32.to_le_bytes());
        image[120..124].copy_from_slice(&1_u32.to_le_bytes());
        image[132..136].copy_from_slice(&unresolved_cgo_count.to_le_bytes());
    }

    /// Minimal version-6 image: one package, one declaration, two
    /// unresolved-cgo cells before the atom plane.
    fn image_with_unresolved_cgo() -> Vec<u8> {
        let cgo_plane = 16;
        let body = DECLARATION_BYTES_V6 + PACKAGE_BYTES + cgo_plane + ATOMS.len();
        let mut image = vec![0_u8; HEADER_BYTES + body];
        write_header(&mut image, body, 2);
        let mut cursor = HEADER_BYTES;
        image[cursor..cursor + DECLARATION_BYTES_V6]
            .copy_from_slice(declaration_row().as_slice());
        cursor += DECLARATION_BYTES_V6;
        image[cursor..cursor + PACKAGE_BYTES].copy_from_slice(package_row().as_slice());
        cursor += PACKAGE_BYTES;
        image[cursor..cursor + 8].copy_from_slice(&cells(&[33, 9]));
        image[cursor + 8..cursor + 16].copy_from_slice(&cells(&[43, 20]));
        cursor += cgo_plane;
        image[cursor..cursor + ATOMS.len()].copy_from_slice(ATOMS);
        reseal(&mut image);
        image
    }

    #[test]
    fn unresolved_cgo_plane_round_trips_both_names() {
        let bytes = image_with_unresolved_cgo();
        let image = GoImage::open(&bytes).expect("open");
        assert_eq!(image.unresolved_cgo_count(), 2);
        assert_eq!(image.unresolved_cgo(0).expect("first"), b"C.sqlite3");
        assert_eq!(
            image.unresolved_cgo(1).expect("second"),
            b"example.com/cgo.Conn"
        );
    }

    #[test]
    fn zero_unresolved_cgo_count_keeps_child_plane_before_atoms() {
        let body = DECLARATION_BYTES_V6 + PACKAGE_BYTES + ATOMS.len();
        let mut image = vec![0_u8; HEADER_BYTES + body];
        write_header(&mut image, body, 0);
        let mut cursor = HEADER_BYTES;
        image[cursor..cursor + DECLARATION_BYTES_V6]
            .copy_from_slice(declaration_row().as_slice());
        cursor += DECLARATION_BYTES_V6;
        image[cursor..cursor + PACKAGE_BYTES].copy_from_slice(package_row().as_slice());
        cursor += PACKAGE_BYTES;
        image[cursor..cursor + ATOMS.len()].copy_from_slice(ATOMS);
        reseal(&mut image);
        let image = GoImage::open(&image).expect("legacy zero count opens");
        assert_eq!(image.unresolved_cgo_count(), 0);
    }
}
