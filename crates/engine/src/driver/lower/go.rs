//! Projects validated `go/packages` authority images through the shared
//! canonical fact lane at complete semantic fidelity.
//!
//! Every plane the image carries is projected: declaration facts, the
//! recursive type graph, executable signatures with parameter and result
//! carrier facts, resolved call occurrences, doc fragments, and the Go
//! extension pool (signatures, type parameters, fields, method sets, build
//! constraints). Never recovers Go facts by scanning source text or a native
//! parser fallback.
//!
//! Emission is two-pass, declarations-first. Pass one commits every named
//! type (its recursive terminal is the legal diagonal self-nominal) and then
//! every alias, so pass two — fields, interface methods, methods, package
//! functions, constants, and variables — projects every type reference
//! against backward fact ordinals only. Constrained declarations, docs, and
//! references follow in passes three through five.
//!
//! Compound types are interned in the anonymous type-row pool, never as
//! carrier facts. The frozen wire orders anonymous rows before fact rows and
//! rejects forward child targets, which yields one projection law this
//! module obeys everywhere: an anonymous row may reference only earlier
//! anonymous rows, while fact rows may reference anonymous rows and earlier
//! facts. A named reference reached at anonymous depth therefore folds to a
//! typed `Unknown` row retaining the name spelling, and a generic
//! application can be carried only as a fact's own root record.
//!
//! Positions the lane cannot host fold to typed `Unknown` rows that keep
//! their reason cell: universe builtins without a lattice row (`error`,
//! `comparable`) as `NoIrRepresentation`, foreign nominals (other packages)
//! as `UnresolvedExternal`, unconstrained inline interfaces, and depth-limit
//! truncation. Parameter and result carriers borrow exact names from the
//! v5 signature-parameter plane; an absent name and an explicit Go blank
//! (`_`) both project to the canonical blank `_` at signature position zero
//! and to a positional spelling (`_1`, `_2`, …) at every later position, so
//! two blank same-typed carriers in one signature never frame identical
//! coordinate-free identities. Receiver spelling and pointer-receiver bits
//! have no
//! `GoFacts` cell and stay image-only. Module metadata, exact constant
//! values, and receiver spellings remain image-only. An interface method-set
//! row is projected onto that interface when its owner is the interface
//! type row, including a method declared in another package or the
//! universe; a name already contributed by an explicit method is not
//! emitted twice. Package rows are projected below.
//!
//! Interface-satisfaction edges — Go's structural implements relation,
//! proved by the oracle across the whole loaded module — project as
//! oracle-confidence type-reference occurrences owned by the satisfying
//! type's fact, resolving in-package targets to local ordinals and
//! cross-package targets to foreign `go` lineage keys. The oracle records no
//! written relation for a satisfaction edge, so each occurrence anchors on
//! the subject's declared identifier: a version-6 image carries the
//! declaration's exact NAME-TOKEN extent, and the occurrence's
//! owner-relative span is that extent (a real extent, verifiable against
//! the source bytes); without one the occurrence keeps the position-free
//! zero-width spelling at the owner's start.
//!
//! Authority-bound source spans: the image marks every declaration and
//! method row that declares in the exact source file the image is
//! digest-bound to, and carries its full declaration extent. The projector
//! attaches those extents as the facts' primary-source spans, so every
//! occurrence's owner-relative span lifts into an absolute, source-verifiable
//! site under the shared containment law. Rows from sibling files stay
//! source-less rather than borrowing another file's coordinates.

use backend_frontend_go::legacy::{
    ChanDir, Declaration, DeclarationKind, DocOwner, GoImage, HeaderError, ImageError, MemberKind,
    ReferenceTargetClass, ReferenceUseKind, TypeRowKind, parse_constraint_blob,
};
use backend_semantic::ir::{
    AtomListId, ChannelDirection, DocFragmentInput, EntityId, EntityKind, EntityListId, ForeignKey,
    ForeignKeyFault, ForeignOrigin, GoFacts, GoSignature, NominalRef, Occurrence,
    OccurrenceConfidence, OccurrenceTarget, PackageLineage, PackageLineageFault, ProductChildRole,
    ReferenceKind, RelSpan, RelSpanFault, SemanticProductConstructor, SemanticTypeChild,
    SemanticTypeRecord, SemanticTypeTag, TypeListId, TypeParameterListId, TypeReason, TypeWidth,
};
use backend_semantic::vocabulary::{
    GoImageDeclarationKind, GoImageDocOwnerKind, GoImageFault, GoImageFlagCell, GoImageHeaderFault,
    GoImageMemberKind, GoImagePlane, GoImageTypeKind,
    GoProjectionFault as PortableGoProjectionFault, GoProjectionIndexPhase, GoProjectionListPhase,
    LoweringUnsupported, ProjectionForeignKeyFault, ProjectionLineagePart,
    ProjectionPackageLineageFault,
};
use core::str;
use sha2::{Digest, Sha256};

use crate::driver::lower::{
    EmissionExtension, FactSet, LEAF_PRODUCT, MAX_REF_LIST_ELEMENTS, SemanticFact,
    StagedSourceSpan, push_fact,
};
use crate::driver::types::{FactFault, FactRejection};

/// Exact rejection while borrowing one validated Go authority image.
///
/// The shared driver failure match owns the terminal arms and lies outside
/// this module's ownership, so this enum keeps exactly the three variants
/// the frozen match already names; every projection fault folds onto
/// [`GoCollectError::Lowering`] at the boundary with its operands retained
/// in [`ProjectionFault`].
#[derive(Debug)]
pub(crate) enum GoCollectError {
    /// The fixed binary image failed structural or checksum validation.
    Image(ImageError),
    /// The image's producer-bound source digest differs from the compile source.
    SourceBinding {
        /// SHA-256 of the exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 lent by the Go authority image header.
        observed: [u8; 32],
    },
    /// The bounded canonical lane cannot admit every authority fact.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
}

/// Exact projection fault retained until the collect boundary folds it into
/// the lane's closed terminal. Operands stay named so the fold site remains
/// typed; widening [`GoCollectError`] requires extending the frozen driver
/// failure match and is recorded as a lane criticism in the module review.
#[derive(Debug)]
enum ProjectionFault {
    /// A validated authority image row could not be reread.
    Image(ImageError),
    /// A bounded coordinate or index cannot be represented by the fixed lane.
    IndexCapacity {
        phase: GoProjectionIndexPhase,
        observed: u64,
    },
    /// The recursive type graph exceeded the producer depth budget.
    Depth { type_row: u32 },
    /// The authority marked a callable variadic without a final typed
    /// parameter to own the rest marker.
    VariadicWithoutParameter {
        /// Image signature row carrying the impossible claim.
        signature: u32,
    },
    /// No pushed fact existed to own an anonymous compound row.
    Anchor { owner: u32 },
    /// A bounded pooled field or method list exceeded its bounded width.
    ListCapacity {
        owner: u32,
        phase: GoProjectionListPhase,
    },
    /// A foreign key could not be built for a resolved external target.
    ForeignKey {
        reference: u32,
        cause: ForeignKeyFault,
    },
    /// The `go` package lineage was rejected.
    Lineage {
        reference: u32,
        cause: PackageLineageFault,
    },
    /// An image atom was not UTF-8 although the image validated its planes.
    Utf8 { plane: GoImagePlane, row: u32 },
    /// A reference span was inverted although the image proved containment.
    Span { row: u32, start: u32, end: u32 },
    /// A doc, method, or member row named no pushed owner fact.
    OrphanOwner {
        /// The image row index whose owner was never pushed.
        owner: u32,
    },
    /// Canonical fact admission rejected the exact projected fact.
    Admission {
        fact: u32,
        name_len: u32,
        cause: FactFault,
    },
}

fn go_u32(
    value: usize,
    phase: GoProjectionIndexPhase,
) -> Result<u32, (GoProjectionIndexPhase, u64)> {
    u32::try_from(value).map_err(|_| (phase, crate::driver::lower::portable_count(value)))
}

fn go_plane(plane: &'static str) -> Option<GoImagePlane> {
    match plane {
        "declaration" => Some(GoImagePlane::Declaration),
        "type" => Some(GoImagePlane::Type),
        "type child" => Some(GoImagePlane::TypeChild),
        "method" => Some(GoImagePlane::Method),
        "type parameter" => Some(GoImagePlane::TypeParameter),
        "member" => Some(GoImagePlane::Member),
        "doc" => Some(GoImagePlane::Doc),
        "reference" => Some(GoImagePlane::Reference),
        "reference target" => Some(GoImagePlane::ReferenceTarget),
        "build constraint" => Some(GoImagePlane::BuildConstraint),
        "satisfaction" => Some(GoImagePlane::Satisfaction),
        "satisfaction target" => Some(GoImagePlane::SatisfactionTarget),
        "package" => Some(GoImagePlane::Package),
        "signature parameter" => Some(GoImagePlane::SignatureParameter),
        "method set" => Some(GoImagePlane::MethodSet),
        "module" => Some(GoImagePlane::Module),
        _ => None,
    }
}

fn go_flag_cell(cell: &'static str) -> Option<GoImageFlagCell> {
    match cell {
        "exported" => Some(GoImageFlagCell::Exported),
        "pointer-receiver" => Some(GoImageFlagCell::PointerReceiver),
        "promoted" => Some(GoImageFlagCell::Promoted),
        "embedded" => Some(GoImageFlagCell::Embedded),
        _ => None,
    }
}

const fn go_decl_kind(kind: DeclarationKind) -> GoImageDeclarationKind {
    match kind {
        DeclarationKind::Type => GoImageDeclarationKind::Type,
        DeclarationKind::Alias => GoImageDeclarationKind::Alias,
        DeclarationKind::Function => GoImageDeclarationKind::Function,
        DeclarationKind::Constant => GoImageDeclarationKind::Constant,
        DeclarationKind::Static => GoImageDeclarationKind::Static,
    }
}

const fn go_type_kind(kind: TypeRowKind) -> GoImageTypeKind {
    match kind {
        TypeRowKind::Basic => GoImageTypeKind::Basic,
        TypeRowKind::Named => GoImageTypeKind::Named,
        TypeRowKind::Alias => GoImageTypeKind::Alias,
        TypeRowKind::TypeParam => GoImageTypeKind::TypeParameter,
        TypeRowKind::Pointer => GoImageTypeKind::Pointer,
        TypeRowKind::Slice => GoImageTypeKind::Slice,
        TypeRowKind::Array => GoImageTypeKind::Array,
        TypeRowKind::Map => GoImageTypeKind::Map,
        TypeRowKind::Chan => GoImageTypeKind::Channel,
        TypeRowKind::Func => GoImageTypeKind::Function,
        TypeRowKind::Struct => GoImageTypeKind::Struct,
        TypeRowKind::Interface => GoImageTypeKind::Interface,
        TypeRowKind::Union => GoImageTypeKind::Union,
        TypeRowKind::Tuple => GoImageTypeKind::Tuple,
        TypeRowKind::Invalid => GoImageTypeKind::Invalid,
    }
}

const fn go_member_kind(kind: MemberKind) -> GoImageMemberKind {
    match kind {
        MemberKind::Field => GoImageMemberKind::Field,
        MemberKind::Method => GoImageMemberKind::Method,
    }
}

const fn go_doc_owner_kind(kind: DocOwner) -> GoImageDocOwnerKind {
    match kind {
        DocOwner::Declaration => GoImageDocOwnerKind::Declaration,
        DocOwner::Method => GoImageDocOwnerKind::Method,
        DocOwner::Member => GoImageDocOwnerKind::Member,
        DocOwner::Package => GoImageDocOwnerKind::Package,
    }
}

const fn portable_foreign_key(fault: ForeignKeyFault) -> ProjectionForeignKeyFault {
    match fault {
        ForeignKeyFault::EmptyPath => ProjectionForeignKeyFault::EmptyPath,
        ForeignKeyFault::BackslashInPath => ProjectionForeignKeyFault::BackslashInPath,
    }
}

const fn portable_lineage(fault: PackageLineageFault) -> ProjectionPackageLineageFault {
    match fault {
        PackageLineageFault::EmptyEcosystem => ProjectionPackageLineageFault::EmptyEcosystem,
        PackageLineageFault::EmptyName => ProjectionPackageLineageFault::EmptyPackage,
        PackageLineageFault::SeparatorInEcosystem => {
            ProjectionPackageLineageFault::SeparatorInEcosystem
        }
        PackageLineageFault::SeparatorInName => ProjectionPackageLineageFault::SeparatorInPackage,
        PackageLineageFault::Backslash { segment } => ProjectionPackageLineageFault::Backslash {
            part: match segment {
                0 => ProjectionLineagePart::Ecosystem,
                1 => ProjectionLineagePart::Package,
                other => ProjectionLineagePart::Invalid { segment: other },
            },
        },
    }
}
fn portable_header_fault(
    error: HeaderError,
) -> Result<GoImageHeaderFault, (GoProjectionIndexPhase, u64)> {
    Ok(match error {
        HeaderError::Truncated { actual } => GoImageHeaderFault::Truncated {
            actual: go_u32(actual, GoProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::Magic { found } => GoImageHeaderFault::Magic { found },
        HeaderError::Version { found } => GoImageHeaderFault::Version { found },
        HeaderError::Length { found } => GoImageHeaderFault::Length {
            found: go_u32(found, GoProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::BodyLength { declared, actual } => GoImageHeaderFault::BodyLength {
            declared: go_u32(declared, GoProjectionIndexPhase::ImageHeader)?,
            actual: go_u32(actual, GoProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::Reserved => GoImageHeaderFault::Reserved,
        HeaderError::ModuleCount { found } => GoImageHeaderFault::ModuleCount {
            found: go_u32(found, GoProjectionIndexPhase::ImageHeader)?,
        },
    })
}

fn portable_image_fault(error: ImageError) -> Result<GoImageFault, (GoProjectionIndexPhase, u64)> {
    Ok(match error {
        ImageError::Header(error) => GoImageFault::Header {
            cause: portable_header_fault(error)?,
        },
        ImageError::Digest => GoImageFault::Digest,
        ImageError::RowBounds {
            plane,
            index,
            count,
        } => GoImageFault::RowBounds {
            plane: go_plane(plane).ok_or((GoProjectionIndexPhase::ImageRow, 0))?,
            index: go_u32(index, GoProjectionIndexPhase::ImageRow)?,
            count: go_u32(count, GoProjectionIndexPhase::ImageRow)?,
        },
        ImageError::DeclarationKind { index, found } => GoImageFault::DeclarationKind {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            found,
        },
        ImageError::ExportedFlag { index, found } => GoImageFault::ExportedFlag {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            found,
        },
        ImageError::DeclarationIota { index, found } => GoImageFault::DeclarationIota {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            found,
        },
        ImageError::DeclarationReserved { index } => GoImageFault::DeclarationReserved {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
        },
        ImageError::TypeReserved { index } => GoImageFault::TypeReserved {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MethodReserved { index } => GoImageFault::MethodReserved {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
        },
        ImageError::MemberReserved { index } => GoImageFault::MemberReserved {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
        },
        ImageError::DocReserved { index } => GoImageFault::DocReserved {
            index: go_u32(index, GoProjectionIndexPhase::Documentation)?,
        },
        ImageError::ReferenceReserved { index } => GoImageFault::ReferenceReserved {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
        },
        ImageError::DeclarationTypeRoot {
            index,
            root,
            type_count,
        } => GoImageFault::DeclarationTypeRoot {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            root,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::DeclarationSpan { index, start, end } => GoImageFault::DeclarationSpan {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            start,
            end,
        },
        ImageError::TypeKind { index, found } => GoImageFault::TypeKind {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            found,
        },
        ImageError::TypeNameRequired { index, kind } => GoImageFault::TypeNameRequired {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            kind: go_type_kind(kind),
        },
        ImageError::TypeNameForbidden { index, kind } => GoImageFault::TypeNameForbidden {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            kind: go_type_kind(kind),
        },
        ImageError::TypeDirection { index, found } => GoImageFault::TypeDirection {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            found,
        },
        ImageError::TypeDirectionCell { index, kind } => GoImageFault::TypeDirectionCell {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            kind: go_type_kind(kind),
        },
        ImageError::TypeVariadicFlag { index, found } => GoImageFault::TypeVariadicFlag {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            found,
        },
        ImageError::TypeVariadicCell { index, kind } => GoImageFault::TypeVariadicCell {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            kind: go_type_kind(kind),
        },
        ImageError::ArrayLength { index, length } => GoImageFault::ArrayLength {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            length,
        },
        ImageError::TypeParamCount {
            index,
            kind,
            param_count,
            child_count,
        } => GoImageFault::TypeParamCount {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            kind: go_type_kind(kind),
            param_count,
            child_count,
        },
        ImageError::TypeChildRange {
            index,
            start,
            count,
            child_count,
        } => GoImageFault::TypeChildRange {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            start: go_u32(start, GoProjectionIndexPhase::TypeChild)?,
            count: go_u32(count, GoProjectionIndexPhase::TypeChild)?,
            child_count: go_u32(child_count, GoProjectionIndexPhase::TypeChild)?,
        },
        ImageError::TypeChildTarget {
            index,
            target,
            type_count,
        } => GoImageFault::TypeChildTarget {
            index: go_u32(index, GoProjectionIndexPhase::TypeChild)?,
            target,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::TypeChildFlags { index, flags } => GoImageFault::TypeChildFlags {
            index: go_u32(index, GoProjectionIndexPhase::TypeChild)?,
            flags,
        },
        ImageError::TypeChildTiling { declared, plane } => GoImageFault::TypeChildTiling {
            declared: go_u32(declared, GoProjectionIndexPhase::TypeChild)?,
            plane: go_u32(plane, GoProjectionIndexPhase::TypeChild)?,
        },
        ImageError::TypeMemberRange {
            index,
            start,
            count,
            member_count,
        } => GoImageFault::TypeMemberRange {
            index: go_u32(index, GoProjectionIndexPhase::TypeRow)?,
            start: go_u32(start, GoProjectionIndexPhase::Member)?,
            count: go_u32(count, GoProjectionIndexPhase::Member)?,
            member_count: go_u32(member_count, GoProjectionIndexPhase::Member)?,
        },
        ImageError::MethodOwner {
            index,
            owner,
            declaration_count,
        } => GoImageFault::MethodOwner {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
            owner,
            declaration_count: go_u32(declaration_count, GoProjectionIndexPhase::Declaration)?,
        },
        ImageError::MethodFlag { index, cell, found } => GoImageFault::MethodFlag {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
            cell: go_flag_cell(cell).ok_or((GoProjectionIndexPhase::Method, 0))?,
            found,
        },
        ImageError::MethodTypeRoot {
            index,
            root,
            type_count,
        } => GoImageFault::MethodTypeRoot {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
            root,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MethodReceiverParams {
            index,
            count,
            blob_bytes,
        } => GoImageFault::MethodReceiverParams {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
            count,
            blob_bytes,
        },
        ImageError::MethodSort {
            index,
            owner,
            previous,
        } => GoImageFault::MethodSort {
            index: go_u32(index, GoProjectionIndexPhase::Method)?,
            owner,
            previous,
        },
        ImageError::TypeParameterOwner {
            index,
            owner,
            declaration_count,
        } => GoImageFault::TypeParameterOwner {
            index: go_u32(index, GoProjectionIndexPhase::TypeParameter)?,
            owner,
            declaration_count: go_u32(declaration_count, GoProjectionIndexPhase::Declaration)?,
        },
        ImageError::TypeParameterConstraint {
            index,
            root,
            type_count,
        } => GoImageFault::TypeParameterConstraint {
            index: go_u32(index, GoProjectionIndexPhase::TypeParameter)?,
            root,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::TypeParameterSort {
            index,
            owner,
            previous,
        } => GoImageFault::TypeParameterSort {
            index: go_u32(index, GoProjectionIndexPhase::TypeParameter)?,
            owner,
            previous,
        },
        ImageError::MemberKind { index, found } => GoImageFault::MemberKind {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
            found,
        },
        ImageError::MemberFlag { index, cell, found } => GoImageFault::MemberFlag {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
            cell: go_flag_cell(cell).ok_or((GoProjectionIndexPhase::Member, 0))?,
            found,
        },
        ImageError::MemberEmbedded { index } => GoImageFault::MemberEmbedded {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
        },
        ImageError::MemberOwner {
            index,
            owner,
            type_count,
        } => GoImageFault::MemberOwner {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
            owner,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MemberTypeRoot {
            index,
            root,
            type_count,
        } => GoImageFault::MemberTypeRoot {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
            root,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MemberSort {
            index,
            owner,
            previous,
        } => GoImageFault::MemberSort {
            index: go_u32(index, GoProjectionIndexPhase::Member)?,
            owner,
            previous,
        },
        ImageError::MemberOwnerRange {
            owner,
            start,
            count,
            actual_start,
            actual_count,
        } => GoImageFault::MemberOwnerRange {
            owner,
            start: go_u32(start, GoProjectionIndexPhase::Member)?,
            count: go_u32(count, GoProjectionIndexPhase::Member)?,
            actual_start: go_u32(actual_start, GoProjectionIndexPhase::Member)?,
            actual_count: go_u32(actual_count, GoProjectionIndexPhase::Member)?,
        },
        ImageError::DocOwnerKind { index, found } => GoImageFault::DocOwnerKind {
            index: go_u32(index, GoProjectionIndexPhase::Documentation)?,
            found,
        },
        ImageError::DocOwner {
            index,
            owner,
            bound,
        } => GoImageFault::DocOwner {
            index: go_u32(index, GoProjectionIndexPhase::Documentation)?,
            owner,
            bound: go_u32(bound, GoProjectionIndexPhase::Documentation)?,
        },
        ImageError::EmptyDoc { index } => GoImageFault::EmptyDoc {
            index: go_u32(index, GoProjectionIndexPhase::Documentation)?,
        },
        ImageError::DocSort {
            index,
            owner_kind,
            owner,
            previous_kind,
            previous_owner,
        } => GoImageFault::DocSort {
            index: go_u32(index, GoProjectionIndexPhase::Documentation)?,
            owner_kind,
            owner,
            previous_kind,
            previous_owner,
        },
        ImageError::ReferenceOwner {
            index,
            owner,
            declaration_count,
        } => GoImageFault::ReferenceOwner {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
            owner,
            declaration_count: go_u32(declaration_count, GoProjectionIndexPhase::Declaration)?,
        },
        ImageError::ReferenceSpan { index, start, end } => GoImageFault::ReferenceSpan {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
            start,
            end,
        },
        ImageError::ReferenceOwnerUnresolved {
            index,
            owner_bytes,
            function_bytes,
        } => GoImageFault::ReferenceOwnerUnresolved {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
            owner_bytes: go_u32(owner_bytes, GoProjectionIndexPhase::Reference)?,
            function_bytes: go_u32(function_bytes, GoProjectionIndexPhase::Reference)?,
        },
        ImageError::ReferenceOwnerSpan { index } => GoImageFault::ReferenceOwnerSpan {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
        },
        ImageError::ReferenceFile { index } => GoImageFault::ReferenceFile {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
        },
        ImageError::ReferenceContainment {
            index,
            start,
            end,
            owner_start,
            owner_end,
        } => GoImageFault::ReferenceContainment {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
            start,
            end,
            owner_start,
            owner_end,
        },
        ImageError::ReferenceSort { index } => GoImageFault::ReferenceSort {
            index: go_u32(index, GoProjectionIndexPhase::Reference)?,
        },
        ImageError::EmptyConstraint { index } => GoImageFault::EmptyConstraint {
            index: go_u32(index, GoProjectionIndexPhase::Constraint)?,
        },
        ImageError::ConstraintBlob {
            index,
            count,
            blob_bytes,
        } => GoImageFault::ConstraintBlob {
            index: go_u32(index, GoProjectionIndexPhase::Constraint)?,
            count,
            blob_bytes,
        },
        ImageError::ConstraintSort { index } => GoImageFault::ConstraintSort {
            index: go_u32(index, GoProjectionIndexPhase::Constraint)?,
        },
        ImageError::SatisfactionSubject {
            index,
            subject,
            declaration_count,
        } => GoImageFault::SatisfactionSubject {
            index: go_u32(index, GoProjectionIndexPhase::Satisfaction)?,
            subject,
            declaration_count: go_u32(declaration_count, GoProjectionIndexPhase::Declaration)?,
        },
        ImageError::SatisfactionSort {
            index,
            subject,
            previous,
        } => GoImageFault::SatisfactionSort {
            index: go_u32(index, GoProjectionIndexPhase::Satisfaction)?,
            subject,
            previous,
        },
        ImageError::SatisfactionSubjectKind { index, kind } => {
            GoImageFault::SatisfactionSubjectKind {
                index: go_u32(index, GoProjectionIndexPhase::Satisfaction)?,
                kind: go_decl_kind(kind),
            }
        }
        ImageError::ModulePath => GoImageFault::ModulePath,
        ImageError::PackageFiles {
            index,
            count,
            blob_bytes,
        } => GoImageFault::PackageFiles {
            index: go_u32(index, GoProjectionIndexPhase::Package)?,
            count,
            blob_bytes,
        },
        ImageError::PackageSort { index } => GoImageFault::PackageSort {
            index: go_u32(index, GoProjectionIndexPhase::Package)?,
        },
        ImageError::DeclarationPackage {
            index,
            package_count,
        } => GoImageFault::DeclarationPackage {
            index: go_u32(index, GoProjectionIndexPhase::Declaration)?,
            package_count: go_u32(package_count, GoProjectionIndexPhase::Package)?,
        },
        ImageError::SignatureParameterPosition { index } => {
            GoImageFault::SignatureParameterPosition {
                index: go_u32(index, GoProjectionIndexPhase::SignatureParameter)?,
            }
        }
        ImageError::MethodSetOwner {
            index,
            owner,
            type_count,
        } => GoImageFault::MethodSetOwner {
            index: go_u32(index, GoProjectionIndexPhase::MethodSet)?,
            owner,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MethodSetOwnerKind { index, kind } => GoImageFault::MethodSetOwnerKind {
            index: go_u32(index, GoProjectionIndexPhase::MethodSet)?,
            kind: go_type_kind(kind),
        },
        ImageError::MethodSetTypeRoot {
            index,
            root,
            type_count,
        } => GoImageFault::MethodSetTypeRoot {
            index: go_u32(index, GoProjectionIndexPhase::MethodSet)?,
            root,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::MethodSetSort { index } => GoImageFault::MethodSetSort {
            index: go_u32(index, GoProjectionIndexPhase::MethodSet)?,
        },
        ImageError::SignatureParameterOwner {
            index,
            owner,
            type_count,
        } => GoImageFault::SignatureParameterOwner {
            index: go_u32(index, GoProjectionIndexPhase::SignatureParameter)?,
            owner,
            type_count: go_u32(type_count, GoProjectionIndexPhase::TypeRow)?,
        },
        ImageError::SignatureParameterOwnerRow {
            index,
            owner,
            expected,
        } => GoImageFault::SignatureParameterOwnerRow {
            index: go_u32(index, GoProjectionIndexPhase::SignatureParameter)?,
            owner,
            expected,
        },
        ImageError::SignatureParameterOrdinal {
            index,
            ordinal,
            expected,
        } => GoImageFault::SignatureParameterOrdinal {
            index: go_u32(index, GoProjectionIndexPhase::SignatureParameter)?,
            ordinal,
            expected,
        },
        ImageError::SignatureParameterTiling { declared, plane } => {
            GoImageFault::SignatureParameterTiling {
                declared: go_u32(declared, GoProjectionIndexPhase::SignatureParameter)?,
                plane: go_u32(plane, GoProjectionIndexPhase::SignatureParameter)?,
            }
        }
        ImageError::EmptyName { plane, index } => GoImageFault::EmptyName {
            plane: go_plane(plane).ok_or((GoProjectionIndexPhase::ImageRow, 0))?,
            index: go_u32(index, GoProjectionIndexPhase::ImageRow)?,
        },
        ImageError::AtomRange {
            plane,
            index,
            offset,
            length,
            atom_bytes,
        } => GoImageFault::AtomRange {
            plane: go_plane(plane).ok_or((GoProjectionIndexPhase::Atom, 0))?,
            index: go_u32(index, GoProjectionIndexPhase::Atom)?,
            offset: go_u32(offset, GoProjectionIndexPhase::Atom)?,
            length: go_u32(length, GoProjectionIndexPhase::Atom)?,
            atom_bytes: go_u32(atom_bytes, GoProjectionIndexPhase::Atom)?,
        },
        ImageError::AtomUtf8 { plane, index } => GoImageFault::AtomUtf8 {
            plane: go_plane(plane).ok_or((GoProjectionIndexPhase::Atom, 0))?,
            index: go_u32(index, GoProjectionIndexPhase::Atom)?,
        },
    })
}

/// Folds one projection fault into the lane's closed terminal. The shared
/// driver failure match owns the terminal arms and is outside this module's
/// ownership, so operand-preserving Go terminals stay folded here.
fn terminal(fault: ProjectionFault) -> GoCollectError {
    let fault = match fault {
        ProjectionFault::Image(image) => match portable_image_fault(image) {
            Ok(cause) => PortableGoProjectionFault::Image { cause },
            Err((phase, observed)) => PortableGoProjectionFault::IndexCapacity { phase, observed },
        },
        ProjectionFault::IndexCapacity { phase, observed } => {
            PortableGoProjectionFault::IndexCapacity { phase, observed }
        }
        ProjectionFault::Depth { type_row } => PortableGoProjectionFault::Depth { type_row },
        ProjectionFault::VariadicWithoutParameter { signature } => {
            PortableGoProjectionFault::VariadicWithoutParameter { signature }
        }
        ProjectionFault::Anchor { owner } => PortableGoProjectionFault::Anchor { owner },
        ProjectionFault::ListCapacity { owner, phase } => {
            PortableGoProjectionFault::ListCapacity { owner, phase }
        }
        ProjectionFault::ForeignKey { reference, cause } => PortableGoProjectionFault::ForeignKey {
            reference,
            cause: portable_foreign_key(cause),
        },
        ProjectionFault::Lineage { reference, cause } => {
            PortableGoProjectionFault::PackageLineage {
                reference,
                cause: portable_lineage(cause),
            }
        }
        ProjectionFault::Utf8 { plane, row } => PortableGoProjectionFault::AtomUtf8 { plane, row },
        ProjectionFault::Span { row, start, end } => {
            PortableGoProjectionFault::RelativeSpan { row, start, end }
        }
        ProjectionFault::OrphanOwner { owner } => PortableGoProjectionFault::OrphanOwner { owner },
        ProjectionFault::Admission {
            fact,
            name_len,
            cause,
        } => PortableGoProjectionFault::Admission {
            fact,
            name_len,
            cause: crate::driver::lower::portable_admission(cause),
        },
    };
    GoCollectError::Lowering(LoweringUnsupported::GoProjection { fault })
}

/// Folds one bounded-lane fact rejection into the lane's closed terminal.
fn lane_terminal(fact: usize, name_len: usize, fault: FactFault) -> GoCollectError {
    let fact = match go_u32(fact, GoProjectionIndexPhase::FactOrdinal) {
        Ok(value) => value,
        Err((phase, observed)) => {
            return terminal(ProjectionFault::IndexCapacity { phase, observed });
        }
    };
    let name_len = match go_u32(name_len, GoProjectionIndexPhase::Atom) {
        Ok(value) => value,
        Err((phase, observed)) => {
            return terminal(ProjectionFault::IndexCapacity { phase, observed });
        }
    };
    terminal(ProjectionFault::Admission {
        fact,
        name_len,
        cause: fault,
    })
}

/// Admission fold for metadata that already names an emitted fact ordinal.
/// A metadata rejection belongs to that owner, not to the current append
/// cursor, so its portable context remains stable even after later passes.
fn lane_terminal_ordinal(fact: u32, name_len: usize, fault: FactFault) -> GoCollectError {
    let fact = match usize::try_from(fact) {
        Ok(value) => value,
        Err(_) => {
            return terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::FactOrdinal,
                observed: u64::from(fact),
            });
        }
    };
    lane_terminal(fact, name_len, fault)
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, GoCollectError> {
    let ordinal = push_fact(facts, fact).map_err(GoCollectError::Rejected)?;
    u32::try_from(ordinal).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: GoProjectionIndexPhase::FactOrdinal,
            observed: ordinal as u64,
        })
    })
}

/// Producer depth budget of the recursive type graph. Go type cycles always
/// pass through a named intermediary, so a structural cycle is hostile and
/// the budget is the typed defense.
const DEPTH_LIMIT: usize = 64;

/// Carrier-fact name for an unnamed position whose signature coordinate is
/// unavailable, and for an anonymous embedded interface term.
const UNNAMED: &[u8] = b"_";

/// Bounded width of the positional absent-name table. A signature's carrier
/// facts are themselves bounded by the lane's type-child width, far below
/// this, so every representable signature position has a spelling.
const ABSENT_NAME_LIMIT: usize = 128;

/// Coordinate-free spellings for absent parameter and result names, indexed
/// by the carrier's position inside its signature. Position zero keeps the
/// canonical blank `_`; every later position gains its structural index, so
/// two absent same-typed carriers in one signature frame distinct identities
/// without inventing a source position or a semantic name. The spellings are
/// static so they outlive every borrowing fact set.
static ABSENT_NAMES: [[u8; 8]; ABSENT_NAME_LIMIT] = build_absent_names();

const fn build_absent_names() -> [[u8; 8]; ABSENT_NAME_LIMIT] {
    let mut table = [[0_u8; 8]; ABSENT_NAME_LIMIT];
    let mut index = 0;
    while index < ABSENT_NAME_LIMIT {
        table[index][0] = b'_';
        if index != 0 {
            let mut value = index;
            let mut reversed = [0_u8; 3];
            let mut digits = 0;
            while value != 0 {
                reversed[digits] = b'0' + (value % 10) as u8;
                value /= 10;
                digits += 1;
            }
            let mut cursor = 0;
            while cursor < digits {
                table[index][1 + cursor] = reversed[digits - 1 - cursor];
                cursor += 1;
            }
        }
        index += 1;
    }
    table
}

/// The exact digit width of one positional absent-name spelling.
const fn absent_name_len(index: usize) -> usize {
    if index == 0 {
        return 1;
    }
    let mut value = index;
    let mut digits = 0;
    while value != 0 {
        digits += 1;
        value /= 10;
    }
    1 + digits
}

/// The static spelling for one absent carrier position, or the canonical
/// blank when the position exceeds the bounded table.
fn absent_name(index: usize) -> &'static [u8] {
    match ABSENT_NAMES.get(index) {
        Some(slot) => &slot[..absent_name_len(index)],
        None => UNNAMED,
    }
}


/// Whether one Go identifier does not bind at its declaration site.
fn is_unbound_name(name: &[u8]) -> bool {
    name.is_empty() || name == b"_"
}

/// Authority-proven discriminator for one unbound declaration whose display
/// spelling would otherwise collide with a sibling at the same scope.
fn unbound_discriminator(index: usize) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"compiler.go.unbound-declaration.v1\0");
    hash.update((index as u64).to_le_bytes());
    let mut discriminator = [0_u8; 16];
    discriminator.copy_from_slice(&hash.finalize()[..16]);
    discriminator
}

/// `PrimitiveShape::Integer` wire cell.
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::Str` wire cell.
const SHAPE_STR: u32 = 4;
/// `PrimitiveShape::MutPointer` wire cell.
const SHAPE_MUT_POINTER: u32 = 5;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;
/// `PrimitiveShape::ArbitraryInteger` wire cell.
const SHAPE_ARBITRARY_INTEGER: u32 = 13;
/// `PrimitiveShape::NativeSignedInteger` wire cell.
const SHAPE_NATIVE_SIGNED_INTEGER: u32 = 14;
/// `PrimitiveShape::NativeUnsignedInteger` wire cell.
const SHAPE_NATIVE_UNSIGNED_INTEGER: u32 = 15;
/// `PrimitiveShape::PointerAddressInteger` wire cell.
const SHAPE_POINTER_ADDRESS_INTEGER: u32 = 16;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// Go ecosystem name of every foreign package lineage.
const ECOSYSTEM: &str = "go";

/// Streams the complete Go semantic plane — declarations, recursive types,
/// signatures, occurrences, docs, and the Go extension pool — into the
/// shared fact lane.
pub(crate) fn collect<'source>(
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), GoCollectError> {
    let image = GoImage::open(image_bytes).map_err(GoCollectError::Image)?;
    let expected = Sha256::digest(source).into();
    let observed = image.source_digest();
    if observed != expected {
        return Err(GoCollectError::SourceBinding { expected, observed });
    }
    // Intern the three empty pooled lists first so the default
    // `::new(0)` coordinates always name genuine empty lists.
    let _ = facts
        .intern_atom_list(&[])
        .map_err(|fault| lane_terminal(facts.len(), 0, fault))?;
    let _ = facts
        .intern_type_list(&[])
        .map_err(|fault| lane_terminal(facts.len(), 0, fault))?;
    let _ = facts
        .intern_entity_list(&[])
        .map_err(|fault| lane_terminal(facts.len(), 0, fault))?;

    let mut go = Projector::new(image, facts);
    // Pass one: named types, then aliases — the backward anchors every
    // later type reference resolves against.
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        if declaration.kind == DeclarationKind::Type {
            go.named_type(index, &declaration)?;
        }
    }
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        if declaration.kind == DeclarationKind::Alias {
            go.alias(index, &declaration)?;
        }
    }
    // Pass two: member planes, executables, and values in producer order.
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        match declaration.kind {
            DeclarationKind::Type => go.type_family(index, &declaration)?,
            DeclarationKind::Function => go.function(index, &declaration)?,
            DeclarationKind::Constant | DeclarationKind::Static => go.value(index, &declaration)?,
            DeclarationKind::Alias => {}
        }
    }
    go.unresolved_cgo()?;
    // Pass three: declarations excluded by build constraints.
    go.constraints()?;
    // Pass four: documentation fragments.
    go.docs()?;
    // Pass five: resolved call occurrences.
    go.occurrences()?;
    // Pass six: interface-satisfaction edges as type-reference occurrences.
    go.satisfactions()?;
    Ok(())
}

/// Maps one closed declaration kind onto the canonical entity kind.
const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Type => EntityKind::Record,
        DeclarationKind::Alias => EntityKind::Alias,
        DeclarationKind::Function => EntityKind::Function,
        DeclarationKind::Constant => EntityKind::Constant,
        DeclarationKind::Static => EntityKind::Static,
    }
}

impl<'image> Projector<'image, '_> {
    /// The canonical kind of one pass-one named type: a declared type whose
    /// root type row is an interface projects as the canonical trait, and
    /// every other defined type (struct or basic-rooted) projects as a
    /// record. A missing root row stays a record — the closed default.
    fn named_type_kind(&self, declaration: &Declaration<'_>) -> Result<EntityKind, GoCollectError> {
        let interface = declaration
            .type_root
            .map(|root| self.image.type_row(index_of(root)).map(|row| row.kind))
            .transpose()
            .map_err(GoCollectError::Image)?
            .is_some_and(|kind| kind == TypeRowKind::Interface);
        Ok(if interface {
            EntityKind::Trait
        } else {
            EntityKind::Record
        })
    }
}

/// The canonical reference kind of one reference row, from its closed use
/// kind and the used object's closed class. A method use is a method call
/// whether the source called it (`x.Close()`) or bound it as a method value
/// (`f := x.Close`) — the lane's closed vocabulary carries both as the
/// method reference they are. A call through a field (`x.handler()`) is a
/// use of the field; value reads of variables, constants, and functions are
/// variable uses.
const fn occurrence_kind(use_kind: ReferenceUseKind, class: ReferenceTargetClass) -> ReferenceKind {
    use ReferenceTargetClass as Class;
    use ReferenceUseKind as Use;
    match (use_kind, class) {
        (Use::Import, _) => ReferenceKind::Import,
        (Use::TypeRef, _) => ReferenceKind::TypeReference,
        (Use::Call, Class::Method) => ReferenceKind::MethodCall,
        (Use::Call, Class::Field) => ReferenceKind::FieldAccess,
        (Use::Call, _) => ReferenceKind::FunctionCall,
        (Use::Read, Class::Method) => ReferenceKind::MethodCall,
        (Use::Read, Class::Field) => ReferenceKind::FieldAccess,
        (Use::Read, _) => ReferenceKind::VariableUse,
    }
}

/// The foreign-key entity kind one reference row's closed target class
/// demands.
const fn foreign_entity_kind(class: ReferenceTargetClass) -> EntityKind {
    match class {
        ReferenceTargetClass::Const => EntityKind::Constant,
        ReferenceTargetClass::Field => EntityKind::Field,
        ReferenceTargetClass::Func | ReferenceTargetClass::Method => EntityKind::Function,
        ReferenceTargetClass::Pkg => EntityKind::Module,
        ReferenceTargetClass::Type => EntityKind::Record,
        ReferenceTargetClass::Var => EntityKind::Static,
    }
}

/// Builds the typed foreign target key for one reference row: the `go`
/// package lineage plus the exact target spelling, under the entity kind
/// the row's closed target class demands. Unresolved and external uses stay
/// exactly this — a resolvable, untruncated key — never a dropped row.
fn foreign_target<'source>(
    reference: u32,
    package: &'source [u8],
    target: &'source [u8],
    kind: EntityKind,
) -> Result<OccurrenceTarget<'source>, GoCollectError> {
    let package = str::from_utf8(package).map_err(|_| {
        terminal(ProjectionFault::Utf8 {
            plane: GoImagePlane::ReferenceTarget,
            row: reference,
        })
    })?;
    let name = str::from_utf8(target).map_err(|_| {
        terminal(ProjectionFault::Utf8 {
            plane: GoImagePlane::ReferenceTarget,
            row: reference,
        })
    })?;
    let lineage = PackageLineage::new(ECOSYSTEM, package)
        .map_err(|cause| ProjectionFault::Lineage { reference, cause })
        .map_err(terminal)?;
    let key = ForeignKey::new(ForeignOrigin::Package(lineage), name, name, Some(kind))
        .map_err(|cause| ProjectionFault::ForeignKey { reference, cause })
        .map_err(terminal)?;
    Ok(OccurrenceTarget::Foreign(key))
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record => SemanticProductConstructor::PRODUCT,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Module
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter
        | EntityKind::Macro
        | EntityKind::Namespace => LEAF_PRODUCT,
    }
}

/// The typed unknown record for one reason and optional spelling. `Unknown`
/// rows whose reason carries a spelling require the text cell; the others
/// forbid it.
const fn unknown_record(reason: TypeReason, spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = match reason {
        TypeReason::Unannotated => 0,
        TypeReason::DynamicallyTyped => 1,
        TypeReason::UnresolvedLocalName => 2,
        TypeReason::UnresolvedExternal => 3,
        TypeReason::TruncatedAtDepthLimit => 4,
        TypeReason::OracleGap => 5,
        TypeReason::NoIrRepresentation => 6,
    };
    record.text = spelling;
    record
}

/// The recursive terminal: a nominal row naming the fact's own ordinal.
const fn nominal_record(ordinal: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
    record
}

/// One projected type prepared for a fact: the lattice record plus its
/// ordered child coordinates with their anonymous-record labels.
struct RootType<'image> {
    record: SemanticTypeRecord<'image>,
    children: Vec<TypeChild<'image>>,
}

/// One projected child coordinate: an anonymous row or a backward fact.
struct TypeChild<'image> {
    target: u32,
    name: Option<&'image [u8]>,
    flags: u8,
}

impl<'image> RootType<'image> {
    /// Wraps one childless lattice record.
    const fn leaf(record: SemanticTypeRecord<'image>) -> Self {
        Self {
            record,
            children: Vec::new(),
        }
    }

    /// Commits the projection onto one fact under construction.
    fn attach(self, fact: SemanticFact<'image>) -> SemanticFact<'image> {
        let mut fact = fact.typed(self.record);
        for child in self.children {
            fact = fact.type_child(child.target, child.name, child.flags);
        }
        fact
    }
}

/// One anonymous type row under construction: the lattice record plus the
/// child coordinates appended into the pooled lane before interning.
struct AnonRow<'image> {
    record: SemanticTypeRecord<'image>,
    children: Vec<TypeChild<'image>>,
}

/// One owner-qualified anonymous-row memo entry. The coordinate is valid only
/// for the declaration whose pending transaction admitted it; a later owner
/// must never inherit that row's type-fact ownership merely because the image
/// type coordinate happens to be shared.
#[derive(Clone, Copy)]
struct AnonymousMemo {
    owner: u32,
    coordinate: u32,
}

/// One same-package member target: the (package, receiver type, member)
/// spelling of a declared method or field plus its pushed fact ordinal, so
/// reference rows naming members through their receiver type resolve to
/// exact local facts.
#[derive(Clone, Copy)]
struct MemberKey<'source> {
    package: &'source [u8],
    type_name: &'source [u8],
    member: &'source [u8],
    ordinal: u32,
    is_field: bool,
}

/// The two-pass Go projector over one validated authority image.
struct Projector<'x, 'source> {
    image: GoImage<'source>,
    facts: &'x mut FactSet<'source>,
    /// Lane ordinal per image declaration index.
    declaration_ordinals: Vec<Option<u32>>,
    /// Lane ordinal per image method row index.
    method_ordinals: Vec<Option<u32>>,
    /// Lane ordinal per image member row index.
    member_ordinals: Vec<Option<u32>>,
    /// Declared names to already-pushed fact ordinals.
    names: Vec<(&'source [u8], &'source [u8], u32)>,
    /// Memoized anonymous-context coordinates per image type row. Entries are
    /// valid only for the current declaration transaction and are replaced
    /// when the next owner reaches the same image coordinate.
    anonymous: Vec<Option<AnonymousMemo>>,
    /// Primary-source declaration spans keyed by image declaration index:
    /// the full authority-bound declaration extent that the declaration's
    /// occurrences are relative to. Only rows flagged bound carry an entry.
    declaration_spans: Vec<Option<(u32, u32)>>,
    /// Primary-source declaration NAME-TOKEN extents keyed by image
    /// declaration index, for satisfaction occurrences that anchor on the
    /// declared identifier itself.
    name_spans: Vec<Option<(u32, u32)>>,
    /// Same-package member targets, resolved for reference rows naming
    /// methods or fields through their receiver type.
    members: Vec<MemberKey<'source>>,
}

impl<'x, 'source> Projector<'x, 'source> {
    fn new(image: GoImage<'source>, facts: &'x mut FactSet<'source>) -> Self {
        let declarations = image.declaration_count();
        Self {
            declaration_ordinals: vec![None; image.declaration_count()],
            method_ordinals: vec![None; image.method_count()],
            member_ordinals: vec![None; image.member_count()],
            anonymous: vec![None; image.type_count()],
            image,
            facts,
            names: Vec::new(),
            declaration_spans: vec![None; declarations],
            name_spans: vec![None; declarations],
            members: Vec::new(),
        }
    }

    /// Records one pushed declaration name for later resolution.
    fn record_name(&mut self, package: &'source [u8], name: &'source [u8], ordinal: u32) {
        self.names.push((package, name, ordinal));
    }

    /// Resolves one declared name to its pushed fact ordinal.
    fn lookup(&self, package: &[u8], name: &[u8]) -> Option<u32> {
        self.names
            .iter()
            .find(|(known_package, known, _)| *known_package == package && *known == name)
            .map(|(_, _, ordinal)| *ordinal)
    }

    /// Reports whether one unqualified identifier already names a declaration
    /// row in the authority image.
    fn image_declared(&self, name: &[u8]) -> Result<bool, GoCollectError> {
        for index in 0..self.image.declaration_count() {
            let declaration = self
                .image
                .declaration(index)
                .map_err(GoCollectError::Image)?;
            if declaration.name == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Emits one package-level alias per unresolved-cgo name whose
    /// unqualified identifier is not already a declaration in this image.
    fn unresolved_cgo(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.unresolved_cgo_count() {
            let spelling = self
                .image
                .unresolved_cgo(index)
                .map_err(GoCollectError::Image)?;
            let unqualified = match spelling.iter().rposition(|byte| *byte == b'.') {
                Some(index) => &spelling[index + 1..],
                None => spelling,
            };
            if self.image_declared(unqualified)? {
                continue;
            }
            let fact = RootType::leaf(unknown_record(TypeReason::UnresolvedExternal, Some(spelling)))
                .attach(SemanticFact::new(
                    EntityKind::Alias,
                    spelling,
                    constructor(EntityKind::Alias),
                ));
            let ordinal = push(self.facts, fact)?;
            self.facts
                .mark_parentage_root(ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, spelling.len(), fault))?;
        }
        Ok(())
    }

    /// Resolves one receiver-qualified member spelling to its pushed fact
    /// ordinal and class.
    fn lookup_member(
        &self,
        package: &[u8],
        type_name: &[u8],
        member: &[u8],
    ) -> Option<(u32, bool)> {
        self.members
            .iter()
            .find(|key| {
                key.package == package && key.type_name == type_name && key.member == member
            })
            .map(|key| (key.ordinal, key.is_field))
    }

    /// Records one image declaration's primary-source facts: its fact's
    /// source span (the full authority-bound declaration extent, the exact
    /// basis every owned occurrence's relative span is measured from) and
    /// its NAME-TOKEN extent for satisfaction anchoring. Unbound rows —
    /// declarations that declare in a sibling source file — stay
    /// source-less rather than borrowing another file's coordinates.
    fn record_declaration_spans(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
        ordinal: u32,
    ) -> Result<(), GoCollectError> {
        if !declaration.bound {
            return Ok(());
        }
        if let Some((start, end)) = declaration.span {
            self.declaration_spans[index] = Some((start, end));
            let staged = StagedSourceSpan::new(start, end).ok_or_else(|| {
                terminal(ProjectionFault::Span {
                    row: u32::try_from(index).unwrap_or(u32::MAX),
                    start,
                    end,
                })
            })?;
            self.facts
                .attach_source_span(ordinal, staged)
                .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        }
        if let Some((start, end)) = declaration.name_span {
            self.name_spans[index] = Some((start, end));
        }
        Ok(())
    }

    /// Records one image method's primary-source span: the declaring
    /// `func` extent, the exact basis every method-owned occurrence's
    /// relative span is measured from. Unbound methods stay source-less.
    fn record_method_span(
        &mut self,
        method: backend_frontend_go::legacy::MethodRow<'source>,
        ordinal: u32,
    ) -> Result<(), GoCollectError> {
        if !method.bound {
            return Ok(());
        }
        let Some((start, end)) = method.span else {
            return Ok(());
        };
        let staged = StagedSourceSpan::new(start, end).ok_or_else(|| {
            terminal(ProjectionFault::Span {
                row: method.owner,
                start,
                end,
            })
        })?;
        self.facts
            .attach_source_span(ordinal, staged)
            .map_err(|fault| lane_terminal_ordinal(ordinal, method.name.len(), fault))
    }

    /// The pending fact that owns anonymous rows being built immediately
    /// before its push. `FactSet` records this reserved coordinate and proves
    /// it becomes valid when the caller admits that exact next fact.
    fn anchor(&self) -> Result<u32, ProjectionFault> {
        let length = self.facts.len();
        u32::try_from(length).map_err(|_| ProjectionFault::IndexCapacity {
            phase: GoProjectionIndexPhase::FactOrdinal,
            observed: length as u64,
        })
    }

    /// Pass one: one named type with its recursive diagonal self-nominal.
    /// The kind follows the underlying shape: an interface root row is the
    /// canonical trait, every other defined type is a record.
    fn named_type(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let own = u32::try_from(self.facts.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::FactOrdinal,
                observed: self.facts.len() as u64,
            })
        })?;
        let kind = self.named_type_kind(declaration)?;
        let fact =
            SemanticFact::new(kind, declaration.name, constructor(kind)).typed(nominal_record(own));
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass one: one alias whose declared type is its projected target.
    /// Targets referencing later declarations fold to typed unknowns
    /// because the lane admits strictly backward references only.
    fn alias(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let root = self.root(declaration.type_root, TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(
            EntityKind::Alias,
            declaration.name,
            constructor(EntityKind::Alias),
        ));
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass two: one named type's member planes — its type parameters, its
    /// struct fields or interface method signatures, its methods, and the
    /// Go extension row binding them to the type fact.
    fn type_family(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let Some(type_ordinal) = self.declaration_ordinals[index] else {
            let owner = u32::try_from(index).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::Declaration,
                    observed: index as u64,
                })
            })?;
            return Err(terminal(ProjectionFault::OrphanOwner { owner }));
        };
        let parameter_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        self.type_parameters(index)?;
        let type_parameters = self
            .facts
            .type_parameter_range(parameter_start)
            .map_err(|fault| lane_terminal(self.facts.len(), declaration.name.len(), fault))?;
        let mut fields = Vec::new();
        let mut interface_methods = Vec::new();
        if let Some(root_cell) = declaration.type_root {
            let row = self
                .image
                .type_row(index_of(root_cell))
                .map_err(GoCollectError::Image)?;
            match row.kind {
                TypeRowKind::Struct => self.fields(
                    &row,
                    &mut fields,
                    type_ordinal,
                    declaration.package,
                    declaration.name,
                )?,
                TypeRowKind::Interface => self.interface_methods(
                    &row,
                    &mut interface_methods,
                    type_ordinal,
                    declaration.package,
                    declaration.name,
                )?,
                _ => {}
            }
        }
        let mut methods = Vec::new();
        let mut method_names = Vec::new();
        for method_index in 0..self.image.method_count() {
            let method = self
                .image
                .method(method_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(method.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let receiver_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::TypeParameter,
                    observed: self.facts.type_parameter_len as u64,
                })
            })?;
            for name in blank_separated(method.receiver_type_params) {
                self.facts
                    .push_type_parameter(name, None, None)
                    .map_err(|fault| lane_terminal(self.facts.len(), method.name.len(), fault))?;
            }
            let ordinal = self.executable(method.name, method.type_root, receiver_start)?;
            self.facts
                .attach_parent(ordinal, type_ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, method.name.len(), fault))?;
            self.record_method_span(method, ordinal)?;
            self.members.push(MemberKey {
                package: declaration.package,
                type_name: declaration.name,
                member: method.name,
                ordinal,
                is_field: false,
            });
            methods.push(ordinal);
            method_names.push(method.name);
            self.method_ordinals[method_index] = Some(ordinal);
        }
        for (ordinal, name) in interface_methods {
            methods.push(ordinal);
            method_names.push(name);
        }
        for method_set_index in 0..self.image.method_set_count() {
            let method_set = self
                .image
                .method_set(method_set_index)
                .map_err(GoCollectError::Image)?;
            // Owner is the interface type-row index, not the declaration
            // index. The declaring package may be this package, a foreign
            // import path, or empty (universe); the name still belongs to
            // this interface's method set.
            if declaration.type_root != Some(method_set.owner)
                || method_names.contains(&method_set.name)
            {
                continue;
            }
            let ordinal =
                self.executable(method_set.name, method_set.type_root, parameter_start)?;
            self.facts
                .attach_parent(ordinal, type_ordinal)
                .map_err(|fault| lane_terminal_ordinal(ordinal, method_set.name.len(), fault))?;
            self.members.push(MemberKey {
                package: declaration.package,
                type_name: declaration.name,
                member: method_set.name,
                ordinal,
                is_field: false,
            });
            methods.push(ordinal);
            method_names.push(method_set.name);
        }
        let fields_list = self.entity_list(&fields, type_ordinal, GoProjectionListPhase::Entity)?;
        let method_set = self.entity_list(&methods, type_ordinal, GoProjectionListPhase::Entity)?;
        self.facts
            .attach_extension_with_type_parameters(
                usize::try_from(type_ordinal).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::FactOrdinal,
                        observed: type_ordinal as u64,
                    })
                })?,
                EmissionExtension::Go(GoFacts {
                    signature: GoSignature {
                        parameters: TypeListId::new(0),
                        results: TypeListId::new(0),
                        variadic: false,
                    },
                    type_parameters: TypeParameterListId::new(parameter_start),
                    fields: fields_list,
                    method_set,
                    build_constraints: AtomListId::new(0),
                    constant_value: AtomListId::new(0),
                    constant_group: 0,
                    constant_flags: 0,
                }),
                type_parameters,
            )
            .map_err(|fault| lane_terminal_ordinal(type_ordinal, declaration.name.len(), fault))?;
        Ok(())
    }

    /// Pushes the pooled type-parameter rows of one declaration. A
    /// constraint resolves to its fact ordinal only when it is a pure local
    /// named row; inline interface constraints have no fact to name and
    /// stay empty on the pooled row.
    fn type_parameters(&mut self, index: usize) -> Result<(), GoCollectError> {
        let own_package = self
            .image
            .declaration(index)
            .map_err(GoCollectError::Image)?
            .package;
        for parameter_index in 0..self.image.type_parameter_count() {
            let row = self
                .image
                .type_parameter(parameter_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(row.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let constraint = row
                .constraint
                .and_then(|root| self.image.type_row(index_of(root)).ok())
                .filter(|row| {
                    matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
                        && row.children.1 == 0
                        && row.package == own_package
                })
                .and_then(|row| self.lookup(row.package, row.name));
            self.facts
                .push_type_parameter(row.name, constraint, None)
                .map_err(|fault| lane_terminal(self.facts.len(), row.name.len(), fault))?;
        }
        Ok(())
    }

    /// Pushes one fact per struct field, projecting each field type as the
    /// field fact's root record, and registers each field under its
    /// receiver type's spelling for member-target resolution.
    fn fields(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
        fields: &mut Vec<u32>,
        owner: u32,
        package: &'source [u8],
        type_name: &'source [u8],
    ) -> Result<(), GoCollectError> {
        let mut field_index = 0usize;
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Field {
                continue;
            }
            let root = self.root(member.type_root, TypeReason::OracleGap)?;
            let mut fact = root.attach(SemanticFact::new(
                EntityKind::Field,
                member.name,
                LEAF_PRODUCT,
            ));
            if is_unbound_name(member.name) {
                fact = fact.with_identity_discriminator(unbound_discriminator(field_index));
            }
            field_index += 1;
            let ordinal = push(self.facts, fact)?;
            self.facts
                .attach_parent(ordinal, owner)
                .map_err(|fault| lane_terminal_ordinal(ordinal, member.name.len(), fault))?;
            self.members.push(MemberKey {
                package,
                type_name,
                member: member.name,
                ordinal,
                is_field: true,
            });
            fields.push(ordinal);
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pushes one function fact per interface method signature, registering
    /// each under its interface type's spelling for member-target
    /// resolution.
    fn interface_methods(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
        methods: &mut Vec<(u32, &'source [u8])>,
        owner: u32,
        package: &'source [u8],
        type_name: &'source [u8],
    ) -> Result<(), GoCollectError> {
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Method {
                continue;
            }
            let start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: GoProjectionIndexPhase::TypeParameter,
                    observed: self.facts.type_parameter_len as u64,
                })
            })?;
            let ordinal = self.executable(member.name, member.type_root, start)?;
            self.facts
                .attach_parent(ordinal, owner)
                .map_err(|fault| lane_terminal_ordinal(ordinal, member.name.len(), fault))?;
            self.members.push(MemberKey {
                package,
                type_name,
                member: member.name,
                ordinal,
                is_field: false,
            });
            methods.push((ordinal, member.name));
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pass two: one package-level function.
    fn function(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let parameter_start = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        self.type_parameters(index)?;
        let ordinal = self.executable(declaration.name, declaration.type_root, parameter_start)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.package, declaration.name, ordinal);
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass two: one constant or variable with its projected declared type.
    fn value(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let kind = entity_kind(declaration.kind);
        let root = self.root(declaration.type_root, TypeReason::Unannotated)?;
        let (constant_value, constant_group, constant_flags) = if kind == EntityKind::Constant {
            if declaration.value.is_empty() {
                (
                    AtomListId::new(0),
                    declaration.const_group,
                    u32::from(declaration.iota),
                )
            } else {
                let atom = self.facts.intern_atom(declaration.value).map_err(|fault| {
                    lane_terminal(self.facts.len(), declaration.name.len(), fault)
                })?;
                let list = self
                    .facts
                    .intern_atom_list(core::slice::from_ref(&atom))
                    .map_err(|fault| {
                        lane_terminal(self.facts.len(), declaration.name.len(), fault)
                    })?;
                (list, declaration.const_group, u32::from(declaration.iota))
            }
        } else {
            (AtomListId::new(0), 0, 0)
        };
        let empty_type_parameters = u32::try_from(self.facts.type_parameter_len).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeParameter,
                observed: self.facts.type_parameter_len as u64,
            })
        })?;
        let mut fact = root
            .attach(SemanticFact::new(kind, declaration.name, constructor(kind)))
            .with_extension(EmissionExtension::Go(GoFacts {
                signature: GoSignature {
                    parameters: TypeListId::new(0),
                    results: TypeListId::new(0),
                    variadic: false,
                },
                type_parameters: TypeParameterListId::new(empty_type_parameters),
                fields: EntityListId::new(0),
                method_set: EntityListId::new(0),
                build_constraints: AtomListId::new(0),
                constant_value,
                constant_group,
                constant_flags,
            }));
        if declaration.kind == DeclarationKind::Static && is_unbound_name(declaration.name) {
            fact = fact.with_identity_discriminator(unbound_discriminator(index));
        }
        let ordinal = push(self.facts, fact)?;
        self.facts
            .mark_parentage_root(ordinal)
            .map_err(|fault| lane_terminal_ordinal(ordinal, declaration.name.len(), fault))?;
        self.declaration_ordinals[index] = Some(ordinal);
        if !is_unbound_name(declaration.name) {
            self.record_name(declaration.package, declaration.name, ordinal);
        }
        self.record_declaration_spans(index, declaration, ordinal)?;
        Ok(())
    }

    /// Pass three: one fact per declaration excluded by a build constraint,
    /// carrying the normalized constraint expression on its Go extension
    /// row.
    fn constraints(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.constraint_count() {
            let row = self
                .image
                .constraint(index)
                .map_err(GoCollectError::Image)?;
            let owner = go_u32(index, GoProjectionIndexPhase::Constraint).map_err(
                |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
            )?;
            let exported = parse_constraint_blob(row.exported, row.exported_count)
                .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner }))?;
            let atom = self
                .facts
                .intern_atom(row.constraint)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
            // The excluded file is an authority owner the lane emits no entity
            // for, and it is the only thing that keeps mutually-exclusive
            // declarations distinct: `colorable_windows.go` and
            // `colorable_appengine.go` both spell `NewColorableStdout`. The
            // normalized build-constraint expression is coordinate-free and,
            // because two files that share an expression can never both
            // declare one name, a stable digest of it is a collision-free
            // owner identity.
            let mut hasher = Sha256::new();
            hasher.update(b"nudox.go.build-constraint-owner.v1\0");
            hasher.update(row.constraint);
            let mut owner_identity = [0_u8; 16];
            owner_identity.copy_from_slice(&hasher.finalize()[..16]);
            let list = self
                .facts
                .intern_atom_list(core::slice::from_ref(&atom))
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
            let empty_type_parameters =
                u32::try_from(self.facts.type_parameter_len).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::TypeParameter,
                        observed: self.facts.type_parameter_len as u64,
                    })
                })?;
            for declaration in exported {
                let kind = entity_kind(declaration.kind);
                let fact = SemanticFact::new(kind, declaration.name, constructor(kind))
                    .typed(unknown_record(TypeReason::OracleGap, None))
                    .with_extension(EmissionExtension::Go(GoFacts {
                        signature: GoSignature {
                            parameters: TypeListId::new(0),
                            results: TypeListId::new(0),
                            variadic: false,
                        },
                        type_parameters: TypeParameterListId::new(empty_type_parameters),
                        fields: EntityListId::new(0),
                        method_set: EntityListId::new(0),
                        build_constraints: list,
                        constant_value: AtomListId::new(0),
                        constant_group: 0,
                        constant_flags: 0,
                    }));
                let ordinal = push(self.facts, fact)?;
                self.facts
                    .mark_unrepresented_parent(ordinal, owner_identity)
                    .map_err(|fault| {
                        lane_terminal_ordinal(ordinal, declaration.name.len(), fault)
                    })?;
            }
        }
        Ok(())
    }

    /// Pass four: one text run per documentation line, soft-break
    /// separated, owned by the pushed fact of the row's owner. Package-level
    /// doc rows have no lane owner — the fact lane owns no package entity —
    /// so they stay image-only facts like the receiver spellings.
    fn docs(&mut self) -> Result<(), GoCollectError> {
        // The validated image owns the complete declaration/method/member
        // documentation plane. Mark every source declaration before rows are
        // decoded so an empty doc list remains captured truth, while callable
        // carrier facts stay outside this source-declaration relation.
        for owner in self
            .declaration_ordinals
            .iter()
            .chain(self.method_ordinals.iter())
            .chain(self.member_ordinals.iter())
            .filter_map(|ordinal| *ordinal)
        {
            self.facts
                .mark_documentation_captured(owner)
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        for index in 0..self.image.doc_count() {
            let row = self.image.doc(index).map_err(GoCollectError::Image)?;
            let owner = match row.owner_kind {
                DocOwner::Package => continue,
                DocOwner::Declaration => self
                    .declaration_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Method => self
                    .method_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Member => self
                    .member_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
            }
            .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner: row.owner }))?;
            push_doc_lines(self.facts, owner, row.text)
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        Ok(())
    }

    /// Pass five: one oracle-confidence occurrence per reference row. The
    /// image's reference plane carries every go/types use of a keyable
    /// object, each with its closed use kind, the used object's closed
    /// class, and the receiver type name for method and field targets.
    /// Resolution keeps the call-graph law: a same-package target resolves
    /// to its local fact when the lane carries one (package scope by name,
    /// members by receiver type and name); everything else — every foreign
    /// package, every promoted or otherwise unlocalizable member — stays a
    /// typed foreign `go` lineage key. Owners lift relative spans over the
    /// authority-bound source spans attached in passes one and two, so the
    /// shared containment law places every site in the exact source bytes
    /// of the used identifier.
    fn occurrences(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.reference_count() {
            let reference_index =
                go_u32(index, GoProjectionIndexPhase::Reference).map_err(|(phase, observed)| {
                    terminal(ProjectionFault::IndexCapacity { phase, observed })
                })?;
            let row = self.image.reference(index).map_err(GoCollectError::Image)?;
            let owner = if row.owner_is_declaration {
                self.declaration_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            } else {
                self.method_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            }
            .ok_or_else(|| {
                terminal(ProjectionFault::OrphanOwner {
                    owner: row.owner_row,
                })
            })?;
            let kind = occurrence_kind(row.use_kind, row.target_class);
            // The package whose lineage keys an unresolvable target: the
            // target's own declaring package when it declares elsewhere,
            // else the owner's package.
            let owner_package = if row.owner_is_declaration {
                self.image
                    .declaration(index_of(row.owner_row))
                    .map_err(GoCollectError::Image)?
                    .package
            } else {
                let method = self
                    .image
                    .method(index_of(row.owner_row))
                    .map_err(GoCollectError::Image)?;
                self.image
                    .declaration(index_of(method.owner))
                    .map_err(GoCollectError::Image)?
                    .package
            };
            let target = if row.target_class == ReferenceTargetClass::Pkg {
                // An imported package binding: the used object is the
                // package itself, keyed by its import path under the `go`
                // ecosystem. The lane owns no package entities, so an
                // import use is always a typed foreign key.
                let package = if row.target_package.is_empty() {
                    owner_package
                } else {
                    row.target_package
                };
                foreign_target(reference_index, package, row.target, EntityKind::Module)?
            } else if row.target_package.is_empty() {
                let local = self
                    .lookup(owner_package, row.target)
                    .map(|ordinal| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    .or_else(|| {
                        self.lookup_member(owner_package, row.recv_type, row.target)
                            .map(|(ordinal, _)| OccurrenceTarget::Local(EntityId::new(ordinal)))
                    });
                match local {
                    Some(target) => target,
                    None => {
                        // Same-package target with no local fact: promoted
                        // members, blank-keyed fields, or build-excluded
                        // declarations. The key keeps the exact spelling
                        // under the declaring package's lineage.
                        foreign_target(
                            reference_index,
                            owner_package,
                            row.target,
                            foreign_entity_kind(row.target_class),
                        )?
                    }
                }
            } else {
                foreign_target(
                    reference_index,
                    row.target_package,
                    row.target,
                    foreign_entity_kind(row.target_class),
                )?
            };
            let span = RelSpan::new(row.relative.0, row.relative.1)
                .map_err(|fault| match fault {
                    RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                        row: reference_index,
                        start,
                        end,
                    },
                })
                .map_err(terminal)?;
            self.facts
                .push_occurrence(
                    owner,
                    Occurrence {
                        target,
                        kind,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))?;
        }
        Ok(())
    }

    /// Pass six: one oracle-confidence type-reference occurrence per
    /// interface-satisfaction edge, owned by the satisfying type's fact.
    /// In-package targets resolve to their local nominal ordinals;
    /// cross-package targets fold to foreign `go` lineage keys. Go never
    /// writes the satisfaction relation, so the occurrence anchors on the
    /// subject's declared identifier: when the image carries the subject's
    /// authority-bound NAME-TOKEN extent, the occurrence's owner-relative
    /// span is exactly that extent — a real extent whose lifted site
    /// verifies against the declared identifier's source bytes; without
    /// one, the occurrence keeps the position-free zero-width spelling at
    /// the owner's start.
    fn satisfactions(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.satisfaction_count() {
            let reference_index = go_u32(index, GoProjectionIndexPhase::Satisfaction).map_err(
                |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
            )?;
            let row = self
                .image
                .satisfaction(index)
                .map_err(GoCollectError::Image)?;
            let subject_index = index_of(row.subject);
            let subject = self
                .declaration_ordinals
                .get(subject_index)
                .copied()
                .flatten()
                .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner: row.subject }))?;
            // Satisfaction targets are declared in the target package, not
            // necessarily in the satisfying subject's package. An absent
            // target package means the subject's declaration package.
            let subject_package = self
                .image
                .declaration(subject_index)
                .map_err(GoCollectError::Image)?
                .package;
            let target_package = if row.target_package.is_empty() {
                subject_package
            } else {
                row.target_package
            };
            let target = if let Some(ordinal) = self.lookup(target_package, row.target) {
                OccurrenceTarget::Local(EntityId::new(ordinal))
            } else {
                // A satisfaction target is an interface by the plane's
                // construction, so its honest foreign kind is the trait —
                // the same kind a local interface projects.
                foreign_target(
                    reference_index,
                    row.target_package,
                    row.target,
                    EntityKind::Trait,
                )?
            };
            // The NAME-TOKEN extent relative to the declaration-extent basis
            // the subject's source span was attached with. Both spans come
            // from one bound row and the image proves the identifier sits
            // inside its declaration, so the relative span is ordered and
            // contained; any other shape is a typed span fault.
            let span = match (
                self.declaration_spans[subject_index],
                self.name_spans[subject_index],
            ) {
                (Some((base, _)), Some((name_start, name_end))) => {
                    let start = name_start.checked_sub(base).ok_or_else(|| {
                        terminal(ProjectionFault::Span {
                            row: reference_index,
                            start: name_start,
                            end: base,
                        })
                    })?;
                    let end = name_end.checked_sub(base).ok_or_else(|| {
                        terminal(ProjectionFault::Span {
                            row: reference_index,
                            start: name_start,
                            end: base,
                        })
                    })?;
                    RelSpan::new(start, end)
                        .map_err(|fault| match fault {
                            RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                                row: reference_index,
                                start,
                                end,
                            },
                        })
                        .map_err(terminal)?
                }
                _ => RelSpan::new(0, 0)
                    .map_err(|fault| match fault {
                        RelSpanFault::Inverted { start, end } => ProjectionFault::Span {
                            row: reference_index,
                            start,
                            end,
                        },
                    })
                    .map_err(terminal)?,
            };
            self.facts
                .push_occurrence(
                    subject,
                    Occurrence {
                        target,
                        kind: ReferenceKind::TypeReference,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(|fault| lane_terminal_ordinal(subject, 0, fault))?;
        }
        Ok(())
    }

    /// Pushes one executable fact: parameter and result carrier facts
    /// first, then the function fact whose product and function-pointer
    /// children are exactly those carriers, with its signature on the Go
    /// extension row.
    fn executable(
        &mut self,
        name: &'source [u8],
        signature: Option<u32>,
        parameter_start: u32,
    ) -> Result<u32, GoCollectError> {
        let mut carriers: Vec<TypeChild<'source>> = Vec::new();
        let mut parameters = Vec::new();
        let mut results = Vec::new();
        let mut variadic = false;
        let record = match signature {
            None => unknown_record(TypeReason::OracleGap, None),
            Some(row_index) => {
                let row = self
                    .image
                    .type_row(index_of(row_index))
                    .map_err(GoCollectError::Image)?;
                variadic = row.variadic;
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                for (ordinal_in_signature, child) in children.iter().take(param_count).enumerate() {
                    let name = self.signature_parameter_name(row_index, ordinal_in_signature)?;
                    let ordinal = self.carrier(*child, name)?;
                    parameters.push(ordinal);
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: None,
                        flags: 0,
                    });
                }
                for (ordinal_in_signature, child) in children.iter().skip(param_count).enumerate() {
                    let ordinal_in_signature = param_count + ordinal_in_signature;
                    let name = self.signature_parameter_name(row_index, ordinal_in_signature)?;
                    let ordinal = self.carrier(*child, name)?;
                    results.push(ordinal);
                    // Only a source-named result labels its callable slot;
                    // the positional `_`/`_1` carrier spelling is identity,
                    // not a name the declaration wrote.
                    let label = self.source_parameter_name(row_index, ordinal_in_signature)?;
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: label,
                        flags: 0,
                    });
                }
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                record.payload1 = u32::try_from(results.len()).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: GoProjectionIndexPhase::TypeRow,
                        observed: results.len() as u64,
                    })
                })?;
                if variadic {
                    let final_parameter = parameters.len().checked_sub(1).ok_or_else(|| {
                        terminal(ProjectionFault::VariadicWithoutParameter {
                            signature: row_index,
                        })
                    })?;
                    let parameter = carriers.get_mut(final_parameter).ok_or_else(|| {
                        terminal(ProjectionFault::VariadicWithoutParameter {
                            signature: row_index,
                        })
                    })?;
                    record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                    parameter.flags |= SemanticTypeChild::FLAG_REST;
                }
                record
            }
        };
        let parameter_list = self
            .facts
            .intern_type_list(&parameters)
            .map_err(|fault| lane_terminal(self.facts.len(), name.len(), fault))?;
        let result_list = self
            .facts
            .intern_type_list(&results)
            .map_err(|fault| lane_terminal(self.facts.len(), name.len(), fault))?;
        let arity = u32::try_from(parameters.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeRow,
                observed: parameters.len() as u64,
            })
        })?;
        let results_count = u32::try_from(results.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::TypeRow,
                observed: results.len() as u64,
            })
        })?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(arity, results_count),
        )
        .typed(record)
        .with_extension(EmissionExtension::Go(GoFacts {
            signature: GoSignature {
                parameters: parameter_list,
                results: result_list,
                variadic,
            },
            type_parameters: TypeParameterListId::new(parameter_start),
            fields: EntityListId::new(0),
            method_set: EntityListId::new(0),
            build_constraints: AtomListId::new(0),
            constant_value: AtomListId::new(0),
            constant_group: 0,
            constant_flags: 0,
        }));
        for ordinal in &parameters {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        for ordinal in &results {
            fact = fact.child(ProductChildRole::FunctionResult, *ordinal);
        }
        for carrier in carriers {
            fact = fact.type_child(carrier.target, carrier.name, carrier.flags);
        }
        let ordinal = push(self.facts, fact)?;
        // Signature carriers share names and types across functions; bind
        // each to its executable so identical carriers stay distinct.
        for carrier in parameters.iter().chain(results.iter()) {
            self.facts
                .attach_parent(*carrier, ordinal)
                .map_err(|fault| lane_terminal_ordinal(*carrier, name.len(), fault))?;
        }
        Ok(ordinal)
    }

    /// Pushes one parameter or result carrier fact whose root record is the
    /// projected parameter type.
    fn signature_parameter_name(
        &self,
        owner: u32,
        ordinal: usize,
    ) -> Result<&'source [u8], GoCollectError> {
        // An unnamed carrier is absent from the image; a source may also spell
        // one with Go's blank identifier. Both are blanks, so both take the
        // positional spelling: `_` at position zero and `_1`, `_2`, … after
        // it. Without this, a signature such as
        // `filter(_ *state, _ reflect.Type, _, _ reflect.Value)` frames four
        // byte-identical carrier identities and collides as a duplicate.
        Ok(self
            .source_parameter_name(owner, ordinal)?
            .unwrap_or_else(|| absent_name(ordinal)))
    }

    /// The carrier name exactly as the source wrote it; `None` for an
    /// unnamed carrier or Go's blank identifier.
    fn source_parameter_name(
        &self,
        owner: u32,
        ordinal: usize,
    ) -> Result<Option<&'source [u8]>, GoCollectError> {
        let parameter = self
            .image
            .signature_parameters()
            .enumerate()
            .find_map(|(_, row)| match row {
                Ok(row) if row.owner == owner && row.ordinal == ordinal as u32 => {
                    Some(Ok(row.name))
                }
                Ok(_) => None,
                Err(error) => Some(Err(GoCollectError::Image(error))),
            })
            .transpose()?
            .unwrap_or(&[]);
        Ok((!parameter.is_empty() && parameter != b"_").then_some(parameter))
    }

    fn carrier(&mut self, row_index: u32, name: &'source [u8]) -> Result<u32, GoCollectError> {
        let root = self.root(Some(row_index), TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT));
        push(self.facts, fact)
    }

    /// Projects one optional type-row coordinate as a fact's root record.
    /// A missing coordinate folds to the caller's typed unknown reason.
    fn root(
        &mut self,
        cell: Option<u32>,
        missing: TypeReason,
    ) -> Result<RootType<'source>, GoCollectError> {
        let Some(row_index) = cell else {
            return Ok(RootType::leaf(unknown_record(missing, None)));
        };
        self.root_row(row_index, DEPTH_LIMIT)
    }

    /// Projects one image type row in fact-root context: the row's record
    /// becomes the fact's own record and its children may name fact
    /// ordinals as well as anonymous rows.
    fn root_row(
        &mut self,
        row_index: u32,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth {
                type_row: row_index,
            }));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        match row.kind {
            TypeRowKind::Basic => Ok(RootType::leaf(self.basic_leaf(&row))),
            TypeRowKind::Named | TypeRowKind::Alias => self.named_root(&row),
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                Ok(RootType::leaf(record))
            }
            TypeRowKind::Invalid => Ok(RootType::leaf(unknown_record(TypeReason::OracleGap, None))),
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                let length = u64::try_from(row.length).map_err(|_| {
                    terminal(ProjectionFault::Image(ImageError::ArrayLength {
                        index: index_of(row_index),
                        length: row.length,
                    }))
                })?;
                let bytes = length.to_le_bytes();
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                self.unary_root(record, children.first().copied(), depth)
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Map),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Channel);
                        record.payload0 = match row.dir {
                            ChanDir::Both => ChannelDirection::Both as u32,
                            ChanDir::Send => ChannelDirection::Send as u32,
                            ChanDir::Recv => ChannelDirection::Receive as u32,
                        };
                        record
                    },
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                let mut projected = RootType {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                        record.payload1 =
                            u32::try_from(children.len() - param_count).map_err(|_| {
                                lane_terminal(self.facts.len(), 0, FactFault::TypeChildCapacity)
                            })?;
                        if row.variadic {
                            record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                        }
                        record
                    },
                    children: Vec::new(),
                };
                for (position, child) in children.into_iter().enumerate() {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: if row.variadic && position + 1 == param_count {
                            SemanticTypeChild::FLAG_REST
                        } else {
                            0
                        },
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Struct => {
                let mut projected = RootType {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                        SemanticTypeTag::Any,
                    )));
                }
                let mut projected = RootType {
                    record: anonymous_record(ANON_INTERFACE),
                    children: Vec::new(),
                };
                for embedded in embedded_run(self.image, &row) {
                    let embedded = embedded.map_err(GoCollectError::Image)?;
                    let target = self.coordinate(Some(embedded), depth - 1)?;
                    let embedded_row = self
                        .image
                        .type_row(index_of(embedded))
                        .map_err(GoCollectError::Image)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(embedded_name(&embedded_row)),
                        flags: 0,
                    });
                }
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    if member.kind != MemberKind::Method {
                        continue;
                    }
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                // The lattice's union children own no approximation cell,
                // so the image's tilde flags stay behind; the term shapes
                // are exact.
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
        }
    }

    /// Projects one unary root: exactly one child coordinate or the typed
    /// depth-less unknown when the image row lost its element.
    fn unary_root(
        &mut self,
        record: SemanticTypeRecord<'source>,
        child: Option<u32>,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        let mut projected = RootType {
            record,
            children: Vec::new(),
        };
        let target = self.coordinate(child, depth - 1)?;
        projected.children.push(TypeChild {
            target,
            name: None,
            flags: 0,
        });
        Ok(projected)
    }

    /// Projects one named or alias row in fact-root context: a pure local
    /// name becomes its nominal fact ordinal, a local application carries
    /// the base fact and its projected arguments, and every foreign or
    /// universe name folds to the typed unknown that retains its spelling.
    fn named_root(
        &mut self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> Result<RootType<'source>, GoCollectError> {
        if !row.package.is_empty()
            && let Some(base) = self.lookup(row.package, row.name)
        {
            if row.children.1 == 0 {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(base)));
                return Ok(RootType::leaf(record));
            }
            let children = self.row_children(row)?;
            let mut projected = RootType {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children: vec![TypeChild {
                    target: base,
                    name: None,
                    flags: 0,
                }],
            };
            for child in children {
                let target = self.coordinate(Some(child), DEPTH_LIMIT - 1)?;
                projected.children.push(TypeChild {
                    target,
                    name: None,
                    flags: 0,
                });
            }
            return Ok(projected);
        }
        let reason = if row.package.is_empty() {
            if row.name == b"any" {
                return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                    SemanticTypeTag::Any,
                )));
            }
            // A universe named type (`error`, `comparable`) is a builtin
            // with no lattice row, never an unresolved external reference.
            TypeReason::NoIrRepresentation
        } else {
            TypeReason::UnresolvedExternal
        };
        Ok(RootType::leaf(unknown_record(reason, Some(row.name))))
    }

    /// Projects one optional type-row coordinate as a child coordinate: a
    /// local named reference stays a fact ordinal, everything else lives in
    /// the anonymous pool.
    fn coordinate(&mut self, cell: Option<u32>, depth: usize) -> Result<u32, GoCollectError> {
        let Some(row_index) = cell else {
            let anchor = self.anchor().map_err(terminal)?;
            return self.intern_anonymous(
                anchor,
                &AnonRow {
                    record: unknown_record(TypeReason::OracleGap, None),
                    children: Vec::new(),
                },
            );
        };
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth {
                type_row: row_index,
            }));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
            && !row.package.is_empty()
            && row.children.1 == 0
            && let Some(local) = self.lookup(row.package, row.name)
        {
            return Ok(local);
        }
        self.project_anonymous(index_of(row_index), depth)
    }

    /// Projects one image type row in anonymous context, memoized per row:
    /// the result is an anonymous row coordinate whose subtree references
    /// only earlier anonymous rows, because the frozen wire orders the pool
    /// before the fact rows and rejects forward child targets.
    fn project_anonymous(&mut self, row_index: usize, depth: usize) -> Result<u32, GoCollectError> {
        let type_row =
            go_u32(row_index, GoProjectionIndexPhase::TypeRow).map_err(|(phase, observed)| {
                terminal(ProjectionFault::IndexCapacity { phase, observed })
            })?;
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth { type_row }));
        }
        let anchor = self.anchor().map_err(terminal)?;
        if let Some(memoized) = self.anonymous.get(row_index).copied().flatten() {
            if memoized.owner == anchor {
                return Ok(memoized.coordinate);
            }
        }
        let row = self
            .image
            .type_row(row_index)
            .map_err(GoCollectError::Image)?;
        let built = match row.kind {
            TypeRowKind::Basic => AnonRow {
                record: self.basic_leaf(&row),
                children: Vec::new(),
            },
            TypeRowKind::Named | TypeRowKind::Alias => AnonRow {
                record: unknown_record(TypeReason::NoIrRepresentation, Some(row.name)),
                children: Vec::new(),
            },
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                AnonRow {
                    record,
                    children: Vec::new(),
                }
            }
            TypeRowKind::Invalid => AnonRow {
                record: unknown_record(TypeReason::OracleGap, None),
                children: Vec::new(),
            },
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                let length = u64::try_from(row.length).map_err(|_| {
                    terminal(ProjectionFault::Image(ImageError::ArrayLength {
                        index: row_index,
                        length: row.length,
                    }))
                })?;
                let bytes = length.to_le_bytes();
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Map),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Channel);
                        record.payload0 = match row.dir {
                            ChanDir::Both => ChannelDirection::Both as u32,
                            ChanDir::Send => ChannelDirection::Send as u32,
                            ChanDir::Recv => ChannelDirection::Receive as u32,
                        };
                        record
                    },
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .map_err(|_| {
                        terminal(ProjectionFault::IndexCapacity {
                            phase: GoProjectionIndexPhase::TypeRow,
                            observed: u64::from(row.param_count),
                        })
                    })?
                    .min(children.len());
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                record.payload1 = u32::try_from(children.len() - param_count).map_err(|_| {
                    lane_terminal(self.facts.len(), 0, FactFault::TypeChildCapacity)
                })?;
                if row.variadic {
                    record.payload0 = SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG;
                }
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                for (position, child) in children.into_iter().enumerate() {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: if row.variadic && position + 1 == param_count {
                            SemanticTypeChild::FLAG_REST
                        } else {
                            0
                        },
                    });
                }
                built
            }
            TypeRowKind::Struct => {
                let mut built = AnonRow {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = match member.type_root {
                        Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                        None => self.coordinate(None, depth - 1)?,
                    };
                    built.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    AnonRow {
                        record: SemanticTypeRecord::leaf(SemanticTypeTag::Any),
                        children: Vec::new(),
                    }
                } else {
                    let mut built = AnonRow {
                        record: anonymous_record(ANON_INTERFACE),
                        children: Vec::new(),
                    };
                    for embedded in embedded_run(self.image, &row) {
                        let embedded = embedded.map_err(GoCollectError::Image)?;
                        let target = self.project_anonymous(index_of(embedded), depth - 1)?;
                        let embedded_row = self
                            .image
                            .type_row(index_of(embedded))
                            .map_err(GoCollectError::Image)?;
                        built.children.push(TypeChild {
                            target,
                            name: Some(embedded_name(&embedded_row)),
                            flags: 0,
                        });
                    }
                    for member_index in member_run(&row) {
                        let member = self
                            .image
                            .member(member_index)
                            .map_err(GoCollectError::Image)?;
                        if member.kind != MemberKind::Method {
                            continue;
                        }
                        let target = match member.type_root {
                            Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                            None => self.coordinate(None, depth - 1)?,
                        };
                        built.children.push(TypeChild {
                            target,
                            name: Some(member.name),
                            flags: 0,
                        });
                    }
                    built
                }
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
        };
        let coordinate = self.intern_anonymous(anchor, &built)?;
        if let Some(slot) = self.anonymous.get_mut(row_index) {
            *slot = Some(AnonymousMemo {
                owner: anchor,
                coordinate,
            });
        }
        Ok(coordinate)
    }

    /// Appends one anonymous row's children into the pooled lane and interns
    /// the row under the given owner fact.
    fn intern_anonymous(
        &mut self,
        anchor: u32,
        row: &AnonRow<'source>,
    ) -> Result<u32, GoCollectError> {
        for child in &row.children {
            self.facts
                .anonymous_type_child(child.target, child.name, child.flags)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))?;
        }
        let fact_count = u32::try_from(self.facts.len()).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: GoProjectionIndexPhase::FactOrdinal,
                observed: self.facts.len() as u64,
            })
        })?;
        if anchor < fact_count {
            self.facts
                .intern_anonymous_type_row(anchor, row.record)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))
        } else {
            self.facts
                .intern_reserved_anchor_type_row(anchor, row.record)
                .map_err(|fault| lane_terminal(self.facts.len(), 0, fault))
        }
    }

    /// The exact lattice cells of one basic row: exact integer and float
    /// widths, `byte`/`rune` aliases folded to their underlying widths,
    /// complex spellings on the builtin shape, and the untyped constant
    /// kinds with an exact lattice equivalent (Go's untyped integer and rune
    /// constants are exact arbitrary-precision integers). Every other
    /// universe basic (`untyped float`, `untyped nil`, `unsafe.Pointer`) is
    /// a builtin the lattice cannot represent, never an unresolved external,
    /// so it keeps its spelling under `NoIrRepresentation`.
    fn basic_leaf(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> SemanticTypeRecord<'source> {
        let signed = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = (width << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG;
            record
        };
        let unsigned = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = width << INTEGER_WIDTH_SHIFT;
            record
        };
        match row.name {
            b"bool" | b"untyped bool" => {
                SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_BOOL)
            }
            b"string" | b"untyped string" => {
                SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_STR)
            }
            b"untyped int" | b"untyped rune" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_ARBITRARY_INTEGER),
            b"int" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_NATIVE_SIGNED_INTEGER),
            b"uint" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_NATIVE_UNSIGNED_INTEGER),
            b"uintptr" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                .with_shape(SHAPE_POINTER_ADDRESS_INTEGER),
            b"int8" => signed(8),
            b"int16" => signed(16),
            b"int32" | b"rune" => signed(32),
            b"int64" => signed(64),
            b"uint8" | b"byte" => unsigned(8),
            b"uint16" => unsigned(16),
            b"uint32" => unsigned(32),
            b"uint64" => unsigned(64),
            b"float32" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(32).to_cell();
                record
            }
            b"float64" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(64).to_cell();
                record
            }
            b"complex64" | b"complex128" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(row.name);
                record
            }
            _ => unknown_record(TypeReason::NoIrRepresentation, Some(row.name)),
        }
    }

    /// The pooled child-run row indices of one type row, image order.
    fn row_children(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'source>,
    ) -> Result<Vec<u32>, GoCollectError> {
        let start = index_of(row.children.0);
        let count = row.children.1 as usize;
        let mut children = Vec::with_capacity(count);
        for offset in 0..count {
            let (target, _) = self
                .image
                .type_child(start + offset)
                .map_err(GoCollectError::Image)?;
            children.push(target);
        }
        Ok(children)
    }

    /// Interns one pooled entity list under its bounded width.
    fn entity_list(
        &mut self,
        ordinals: &[u32],
        owner: u32,
        phase: GoProjectionListPhase,
    ) -> Result<EntityListId, GoCollectError> {
        if ordinals.len() > MAX_REF_LIST_ELEMENTS {
            return Err(terminal(ProjectionFault::ListCapacity { owner, phase }));
        }
        self.facts
            .intern_entity_list(ordinals)
            .map_err(|fault| lane_terminal_ordinal(owner, 0, fault))
    }
}

/// `AnonRecordForm::Struct` and `::Interface` wire cells.
const ANON_STRUCT: u32 = 0;
const ANON_INTERFACE: u32 = 1;

/// The anonymous-record leaf record for one form cell.
const fn anonymous_record(form: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
    record.payload0 = form;
    record
}

/// `PrimitiveShape` cells attached to a primitive leaf record.
trait WithShape {
    fn with_shape(self, shape: u32) -> Self;
}

impl WithShape for SemanticTypeRecord<'_> {
    fn with_shape(mut self, shape: u32) -> Self {
        self.payload0 = shape;
        self
    }
}

/// The member-run row indices of one struct or interface row.
fn member_run(row: &backend_frontend_go::legacy::TypeRow<'_>) -> Vec<usize> {
    let start = index_of(row.members.0);
    let count = row.members.1 as usize;
    (start..start + count).collect()
}

/// The embedded-run row indices of one interface row.
fn embedded_run<'image>(
    image: GoImage<'image>,
    row: &backend_frontend_go::legacy::TypeRow<'image>,
) -> impl Iterator<Item = Result<u32, ImageError>> + 'image {
    let start = index_of(row.children.0);
    let count = row.children.1 as usize;
    // `GoImage::open` already proved this exact child range. Streaming keeps
    // every target authority-bound without a per-interface allocation.
    (start..start.saturating_add(count))
        .map(move |offset| image.type_child(offset).map(|(target, _)| target))
}

/// The first embedded row's spelling; anonymous embeddeds carry none and
/// take the blank identifier.
fn embedded_name<'image>(row: &backend_frontend_go::legacy::TypeRow<'image>) -> &'image [u8] {
    if row.name.is_empty() {
        UNNAMED
    } else {
        row.name
    }
}

/// Splits one validated NUL-separated name blob into its non-empty parts.
fn blank_separated(blob: &[u8]) -> Vec<&[u8]> {
    blob.split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Converts one validated u32 coordinate to its row index.
fn index_of(coordinate: u32) -> usize {
    coordinate as usize
}

/// Pushes one documentation text run as lines with soft breaks between
/// them; empty lines contribute only their breaks.
fn push_doc_lines<'source>(
    facts: &mut FactSet<'source>,
    owner: u32,
    text: &'source [u8],
) -> Result<(), FactFault> {
    let mut line_start = 0_usize;
    while line_start < text.len() {
        let line_end = text[line_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |at| line_start + at);
        let line = &text[line_start..line_end];
        if !line.is_empty() {
            facts.push_doc(owner, DocFragmentInput::Text(line))?;
        }
        if line_end == text.len() {
            break;
        }
        facts.push_doc(owner, DocFragmentInput::SoftBreak)?;
        line_start = line_end + 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::lower::{FactSet, MAX_REF_LISTS};
    use crate::driver::types::FactFault;
    use backend_semantic::ir::{ChannelDirection, FragmentView, SourceIdentity};
    use backend_semantic::vocabulary::{
        CompileRecipeFact, GoImageFault, GoProjectionFault as PortableGoProjectionFault,
        LanguageProfile, LoweringUnsupported, NativeTool, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    const HEADER_BYTES: usize = 136;
    const NONE: u32 = u32::MAX;
    const IMAGE_DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v6\0";
    const FILE: &[u8] = b"main.go";
    const PACKAGE: &[u8] = b"example.com/demo";
    const SPAN_END: u32 = 256;

    /// Fixture declaration-kind tags.
    const KIND_TYPE: u8 = 1;
    const KIND_ALIAS: u8 = 2;
    const KIND_FUNC: u8 = 3;
    const KIND_CONST: u8 = 4;
    const KIND_VAR: u8 = 5;

    /// Fixture type-row discriminants.
    const ROW_BASIC: u8 = 0;
    const ROW_NAMED: u8 = 1;
    const ROW_TYPE_PARAM: u8 = 3;
    const ROW_POINTER: u8 = 4;
    const ROW_SLICE: u8 = 5;
    const ROW_ARRAY: u8 = 6;
    const ROW_MAP: u8 = 7;
    const ROW_CHAN: u8 = 8;
    const ROW_FUNC: u8 = 9;
    const ROW_STRUCT: u8 = 10;
    const ROW_INTERFACE: u8 = 11;

    /// Fixture doc owner kinds.
    const DOC_DECLARATION: u8 = 0;
    const DOC_MEMBER: u8 = 2;
    const DOC_PACKAGE: u8 = 3;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collection rejected the authority image: {0:?}")]
        Collect(GoCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(crate::driver::lower::AdmissionFault),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(backend_semantic::ir::FragmentError),
        #[error("type fact cursor rejected: {0:?}")]
        TypeFact(backend_semantic::ir::TypeFactFault),
        #[error("occurrence cursor rejected: {0:?}")]
        Occurrence(backend_semantic::ir::OccurrenceFault),
        #[error("documentation cursor rejected: {0:?}")]
        Doc(backend_semantic::ir::DocFactFault),
        #[error("owned image materialization rejected the fact set: {0:?}")]
        Ir(backend_semantic::ir::BuildError),
        #[error("fixture scalar conversion failed: {0}")]
        Num(core::num::TryFromIntError),
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("committed bytes changed")]
        Tail,
    }

    impl From<GoCollectError> for TestError {
        fn from(error: GoCollectError) -> Self {
            Self::Collect(error)
        }
    }

    impl From<backend_semantic::ir::TypeFactFault> for TestError {
        fn from(error: backend_semantic::ir::TypeFactFault) -> Self {
            Self::TypeFact(error)
        }
    }

    impl From<backend_semantic::ir::OccurrenceFault> for TestError {
        fn from(error: backend_semantic::ir::OccurrenceFault) -> Self {
            Self::Occurrence(error)
        }
    }

    impl From<backend_semantic::ir::DocFactFault> for TestError {
        fn from(error: backend_semantic::ir::DocFactFault) -> Self {
            Self::Doc(error)
        }
    }

    impl From<crate::driver::lower::AdmissionFault> for TestError {
        fn from(error: crate::driver::lower::AdmissionFault) -> Self {
            Self::Admission(error)
        }
    }

    impl From<backend_semantic::ir::FragmentError> for TestError {
        fn from(error: backend_semantic::ir::FragmentError) -> Self {
            Self::Validate(error)
        }
    }

    impl From<core::num::TryFromIntError> for TestError {
        fn from(error: core::num::TryFromIntError) -> Self {
            Self::Num(error)
        }
    }

    #[test]
    fn image_projection_retains_exact_row_fault_operands() {
        let error = terminal(ProjectionFault::Image(ImageError::DeclarationKind {
            index: 7,
            found: 0xfe,
        }));
        let GoCollectError::Lowering(LoweringUnsupported::GoProjection {
            fault:
                PortableGoProjectionFault::Image {
                    cause: GoImageFault::DeclarationKind { index, found },
                },
        }) = error
        else {
            panic!("image fault lost its closed Go projection operands");
        };
        assert_eq!((index, found), (7, 0xfe));
    }

    #[test]
    fn admission_projection_retains_exact_pool_operands() {
        let error = lane_terminal(
            17,
            6,
            FactFault::ProductChildPoolCapacity {
                used: 3,
                requested: 5,
                capacity: 7,
            },
        );
        let GoCollectError::Lowering(LoweringUnsupported::GoProjection {
            fault:
                PortableGoProjectionFault::Admission {
                    fact,
                    name_len,
                    cause:
                        backend_semantic::vocabulary::ProjectionAdmissionFault::ProductChildPoolCapacity {
                            used,
                            requested,
                            capacity,
                        },
                },
        }) = error
        else {
            panic!("admission fault lost its exact Go context or pool operands");
        };
        assert_eq!(
            (fact, name_len, used, requested, capacity),
            (17, 6, 3, 5, 7)
        );
    }

    #[test]
    fn metadata_admission_retains_its_known_owner_coordinate() {
        let error = lane_terminal_ordinal(
            23,
            0,
            FactFault::OccurrenceOwner {
                owner: 23,
                fact_count: 29,
            },
        );
        let GoCollectError::Lowering(LoweringUnsupported::GoProjection {
            fault:
                PortableGoProjectionFault::Admission {
                    fact: 23,
                    name_len: 0,
                    cause:
                        backend_semantic::vocabulary::ProjectionAdmissionFault::OccurrenceOwner {
                            owner,
                            fact_count,
                        },
                },
        }) = error
        else {
            panic!("metadata admission lost its known Go owner coordinate");
        };
        assert_eq!((owner, fact_count), (23, 29));
    }

    #[derive(Clone, Copy)]
    struct Cell {
        offset: u32,
        length: u32,
    }

    /// One Go authority image under construction, encoded exactly like the
    /// vendored producer: canonical planes, fixed header, and the
    /// domain-separated source-bound checksum.
    #[derive(Clone, Default)]
    struct Fixture {
        atom_bytes: Vec<u8>,
        declarations: Vec<DeclRow>,
        types: Vec<TypeRowF>,
        methods: Vec<MethodF>,
        parameters: Vec<ParameterF>,
        members: Vec<MemberF>,
        method_sets: Vec<MethodSetF>,
        docs: Vec<DocF>,
        references: Vec<RefF>,
        constraints: Vec<ConstraintF>,
        satisfactions: Vec<SatisfactionF>,
        children: Vec<u32>,
        unresolved_cgo_cells: Vec<Cell>,
        signature_parameter_names: Vec<(u32, u32, Cell)>,
    }

    #[derive(Clone)]
    struct DeclRow {
        kind: u8,
        name: Cell,
        package: Cell,
        type_root: Option<u32>,
        value: Cell,
        const_group: i64,
        iota: bool,
        /// The declaration's full source extent; every declaration carries
        /// one.
        span: (u32, u32),
        /// The declared identifier's absolute byte extent (version 6).
        name_span: Option<(u32, u32)>,
        /// The authority-bound-source flag (version 6).
        bound: bool,
    }

    #[derive(Clone)]
    struct TypeRowF {
        kind: u8,
        dir: u8,
        variadic: u8,
        name: Cell,
        package: Cell,
        length: i64,
        param_count: u32,
        children: (u32, u32),
        members: (u32, u32),
    }

    #[derive(Clone)]
    struct MethodF {
        owner: u32,
        name: Cell,
        type_root: Option<u32>,
        receiver: Cell,
        receiver_params: (Cell, u32),
        /// The method's declaring extent.
        span: (u32, u32),
        /// The authority-bound-source flag (version 6).
        bound: bool,
    }

    #[derive(Clone)]
    struct ParameterF {
        owner: u32,
        name: Cell,
        constraint: Option<u32>,
    }

    #[derive(Clone)]
    struct MemberF {
        owner: u32,
        kind: u8,
        name: Cell,
        type_root: Option<u32>,
    }

    #[derive(Clone)]
    struct MethodSetF {
        owner: u32,
        name: Cell,
        type_root: Option<u32>,
        package: Cell,
    }

    #[derive(Clone)]
    struct DocF {
        owner_kind: u8,
        owner: u32,
        text: Cell,
    }

    #[derive(Clone)]
    struct RefF {
        owner: u32,
        target: Cell,
        target_package: Cell,
        receiver: Cell,
        start: u32,
        end: u32,
        /// The closed use-kind byte (version 6).
        use_kind: u8,
        /// The closed target-class byte (version 6).
        target_class: u8,
        /// The target receiver type-name atom (version 6).
        recv_type: Cell,
    }

    #[derive(Clone)]
    struct ConstraintF {
        file: Cell,
        constraint: Cell,
        blob: Cell,
        count: u32,
    }

    #[derive(Clone)]
    struct SatisfactionF {
        subject: u32,
        target: Cell,
        target_package: Cell,
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Self::default();
            fixture.atom(FILE);
            fixture.atom(b"demo");
            fixture.atom(b"main.go\0");
            fixture.atom(PACKAGE);
            fixture
        }

        fn atom(&mut self, text: &[u8]) -> Cell {
            let offset = u32::try_from(self.atom_bytes.len()).unwrap_or(u32::MAX);
            self.atom_bytes.extend_from_slice(text);
            Cell {
                offset,
                length: u32::try_from(text.len()).unwrap_or(u32::MAX),
            }
        }

        fn blob(&mut self, records: &[(u8, &[u8])]) -> (Cell, u32) {
            let cell = self.atom(&[]);
            let count = u32::try_from(records.len()).unwrap_or(u32::MAX);
            for (kind, name) in records {
                self.atom_bytes.push(*kind);
                self.atom_bytes.extend_from_slice(name);
                self.atom_bytes.push(0);
            }
            (
                Cell {
                    offset: cell.offset,
                    length: u32::try_from(
                        self.atom_bytes.len() - usize::try_from(cell.offset).unwrap_or(0),
                    )
                    .unwrap_or(u32::MAX),
                },
                count,
            )
        }

        fn push_type_row(&mut self, row: TypeRowF) -> u32 {
            let index = u32::try_from(self.types.len()).unwrap_or(u32::MAX);
            self.types.push(row);
            index
        }

        fn start_row(&mut self, kind: u8) -> u32 {
            let index = self.push_type_row(TypeRowF {
                kind,
                dir: 0,
                variadic: 0,
                name: Cell {
                    offset: 0,
                    length: 0,
                },
                package: Cell {
                    offset: 0,
                    length: 0,
                },
                length: 0,
                param_count: 0,
                children: (u32::try_from(self.children.len()).unwrap_or(u32::MAX), 0),
                members: (0, 0),
            });
            index
        }

        fn add_child(&mut self, row_index: u32, target: u32) {
            let row = &mut self.types[index_of(row_index)];
            row.children.1 += 1;
            self.children.push(target);
        }

        fn add_member(&mut self, row_index: u32, member: MemberF) {
            let row = &mut self.types[index_of(row_index)];
            if row.members.1 == 0 {
                row.members.0 = u32::try_from(self.members.len()).unwrap_or(u32::MAX);
            }
            row.members.1 += 1;
            self.members.push(member);
        }

        fn basic(&mut self, name: &[u8]) -> u32 {
            let spelled = self.atom(name);
            let index = self.start_row(ROW_BASIC);
            self.types[index_of(index)].name = spelled;
            index
        }

        fn named(&mut self, package: &[u8], name: &[u8], arguments: &[u32]) -> u32 {
            let spelled = self.atom(name);
            let package = self.atom(package);
            let index = self.start_row(ROW_NAMED);
            let row = &mut self.types[index_of(index)];
            row.name = spelled;
            row.package = package;
            for argument in arguments {
                self.add_child(index, *argument);
            }
            index
        }

        fn type_param_row(&mut self, name: &[u8]) -> u32 {
            let spelled = self.atom(name);
            let index = self.start_row(ROW_TYPE_PARAM);
            self.types[index_of(index)].name = spelled;
            index
        }

        fn unary(&mut self, kind: u8, child: u32) -> u32 {
            let index = self.start_row(kind);
            self.add_child(index, child);
            index
        }

        fn array(&mut self, length: i64, child: u32) -> u32 {
            let index = self.unary(ROW_ARRAY, child);
            self.types[index_of(index)].length = length;
            index
        }

        fn chan(&mut self, dir: u8, child: u32) -> u32 {
            let index = self.unary(ROW_CHAN, child);
            self.types[index_of(index)].dir = dir;
            index
        }

        fn map(&mut self, key: u32, value: u32) -> u32 {
            let index = self.start_row(ROW_MAP);
            self.add_child(index, key);
            self.add_child(index, value);
            index
        }

        fn func(&mut self, parameters: &[u32], results: &[u32], variadic: bool) -> u32 {
            let index = self.start_row(ROW_FUNC);
            let row = &mut self.types[index_of(index)];
            row.variadic = u8::from(variadic);
            row.param_count = u32::try_from(parameters.len()).unwrap_or(u32::MAX);
            for parameter in parameters {
                self.add_child(index, *parameter);
            }
            for result in results {
                self.add_child(index, *result);
            }
            index
        }

        /// Names one signature parameter or result position on a func row.
        fn name_signature_parameter(&mut self, owner: u32, ordinal: u32, name: &[u8]) {
            let cell = self.atom(name);
            self.signature_parameter_names.push((owner, ordinal, cell));
        }

        fn field(&mut self, owner: u32, name: &[u8], type_root: Option<u32>) {
            let spelled = self.atom(name);
            self.add_member(
                owner,
                MemberF {
                    owner,
                    kind: 0,
                    name: spelled,
                    type_root,
                },
            );
        }

        fn interface_method(&mut self, owner: u32, name: &[u8], type_root: Option<u32>) {
            let spelled = self.atom(name);
            self.add_member(
                owner,
                MemberF {
                    owner,
                    kind: 1,
                    name: spelled,
                    type_root,
                },
            );
        }

        fn method_set(&mut self, owner: u32, name: &[u8], type_root: Option<u32>) {
            self.method_set_from(owner, name, type_root, PACKAGE);
        }

        fn method_set_from(
            &mut self,
            owner: u32,
            name: &[u8],
            type_root: Option<u32>,
            package: &[u8],
        ) {
            let name = self.atom(name);
            let package = self.atom(package);
            self.method_sets.push(MethodSetF {
                owner,
                name,
                type_root,
                package,
            });
        }

        fn unresolved_cgo(&mut self, name: &[u8]) {
            let cell = self.atom(name);
            self.unresolved_cgo_cells.push(cell);
        }

        fn declaration(&mut self, kind: u8, name: &[u8], type_root: Option<u32>) -> usize {
            let spelled = self.atom(name);
            let package = self.atom(PACKAGE);
            self.declarations.push(DeclRow {
                kind,
                name: spelled,
                package,
                type_root,
                value: Cell {
                    offset: 0,
                    length: 0,
                },
                const_group: 0,
                iota: false,
                span: (0, SPAN_END),
                name_span: None,
                bound: false,
            });
            self.declarations.len() - 1
        }

        fn constant(
            &mut self,
            name: &[u8],
            type_root: Option<u32>,
            value: &[u8],
            group: i64,
        ) -> usize {
            let index = self.declaration(KIND_CONST, name, type_root);
            let spelled = self.atom(value);
            self.declarations[index].value = spelled;
            self.declarations[index].const_group = group;
            self.declarations[index].iota = true;
            index
        }

        fn satisfaction(&mut self, subject: usize, target: &[u8], target_package: &[u8]) {
            let spelled = self.atom(target);
            let package = self.atom(target_package);
            self.satisfactions.push(SatisfactionF {
                subject: u32::try_from(subject).unwrap_or(u32::MAX),
                target: spelled,
                target_package: package,
            });
        }

        fn method(&mut self, owner: usize, name: &[u8], type_root: Option<u32>) -> usize {
            let spelled = self.atom(name);
            let receiver = self.atom(b"t");
            self.methods.push(MethodF {
                owner: u32::try_from(owner).unwrap_or(u32::MAX),
                name: spelled,
                type_root,
                receiver,
                receiver_params: (
                    Cell {
                        offset: 0,
                        length: 0,
                    },
                    0,
                ),
                span: (0, SPAN_END),
                bound: false,
            });
            self.methods.len() - 1
        }

        fn type_parameter(&mut self, owner: usize, name: &[u8], constraint: Option<u32>) {
            let spelled = self.atom(name);
            self.parameters.push(ParameterF {
                owner: u32::try_from(owner).unwrap_or(u32::MAX),
                name: spelled,
                constraint,
            });
        }

        fn doc(&mut self, owner_kind: u8, owner: u32, text: &[u8]) {
            let borrowed = self.atom(text);
            self.docs.push(DocF {
                owner_kind,
                owner,
                text: borrowed,
            });
        }

        fn reference(
            &mut self,
            owner: u32,
            receiver: &[u8],
            target: &[u8],
            target_package: &[u8],
            start: u32,
            end: u32,
        ) {
            self.reference_typed(
                owner,
                receiver,
                target,
                target_package,
                start,
                end,
                0,
                0,
                b"",
            );
        }

        /// One version-6 reference row: the legacy cells plus the closed
        /// use-kind byte, the target-class byte, and the receiver type-name
        /// atom.
        fn reference_typed(
            &mut self,
            owner: u32,
            receiver: &[u8],
            target: &[u8],
            target_package: &[u8],
            start: u32,
            end: u32,
            use_kind: u8,
            target_class: u8,
            recv_type: &[u8],
        ) {
            let target = self.atom(target);
            let package = self.atom(target_package);
            let receiver = self.atom(receiver);
            let recv_type = self.atom(recv_type);
            self.references.push(RefF {
                owner,
                target,
                target_package: package,
                receiver,
                start,
                end,
                use_kind,
                target_class,
                recv_type,
            });
        }

        fn constraint(&mut self, file: &[u8], constraint: &[u8], exported: &[(u8, &[u8])]) {
            let file = self.atom(file);
            let spelled = self.atom(constraint);
            let (blob, count) = self.blob(exported);
            self.constraints.push(ConstraintF {
                file,
                constraint: spelled,
                blob,
                count,
            });
        }

        fn file_cell(&self) -> Cell {
            self.atom_cell(FILE)
        }

        fn atom_cell(&self, text: &[u8]) -> Cell {
            let offset = self
                .atom_bytes
                .windows(text.len())
                .position(|window| window == text)
                .map_or(u32::MAX, |at| u32::try_from(at).unwrap_or(u32::MAX));
            Cell {
                offset,
                length: u32::try_from(text.len()).unwrap_or(u32::MAX),
            }
        }

        fn encode(&self, source: &[u8]) -> Result<Vec<u8>, TestError> {
            let count = |length: usize| u32::try_from(length).map_err(TestError::from);
            let cell =
                |borrowed: Cell| (borrowed.offset.to_le_bytes(), borrowed.length.to_le_bytes());
            let file = self.file_cell();
            let mut declarations = Vec::new();
            for row in &self.declarations {
                let (name, name_len) = cell(row.name);
                let (package, package_len) = cell(row.package);
                let (value, value_len) = cell(row.value);
                declarations.extend_from_slice(&[row.kind, 1, u8::from(row.iota), 0]);
                declarations.extend_from_slice(&name);
                declarations.extend_from_slice(&name_len);
                declarations.extend_from_slice(&package);
                declarations.extend_from_slice(&package_len);
                declarations.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                declarations.extend_from_slice(&row.span.0.to_le_bytes());
                declarations.extend_from_slice(&row.span.1.to_le_bytes());
                declarations.extend_from_slice(&file.offset.to_le_bytes());
                declarations.extend_from_slice(&file.length.to_le_bytes());
                declarations.extend_from_slice(&value);
                declarations.extend_from_slice(&value_len);
                declarations.extend_from_slice(&row.const_group.to_le_bytes());
                // Version 6: the declared identifier's byte extent and the
                // authority-bound flag.
                let (name_start, name_end) = row.name_span.unwrap_or((NONE, NONE));
                declarations.extend_from_slice(&name_start.to_le_bytes());
                declarations.extend_from_slice(&name_end.to_le_bytes());
                declarations.extend_from_slice(&[u8::from(row.bound), 0, 0, 0, 0, 0, 0, 0]);
            }
            let mut types = Vec::new();
            for row in &self.types {
                let (name, name_len) = cell(row.name);
                let (package, package_len) = cell(row.package);
                types.extend_from_slice(&[row.kind, row.dir, row.variadic, 0]);
                types.extend_from_slice(&name);
                types.extend_from_slice(&name_len);
                types.extend_from_slice(&package);
                types.extend_from_slice(&package_len);
                types.extend_from_slice(&row.length.to_le_bytes());
                types.extend_from_slice(&row.children.0.to_le_bytes());
                types.extend_from_slice(&row.children.1.to_le_bytes());
                types.extend_from_slice(&row.members.0.to_le_bytes());
                types.extend_from_slice(&row.members.1.to_le_bytes());
                types.extend_from_slice(&0_u32.to_le_bytes());
                types.extend_from_slice(&row.param_count.to_le_bytes());
            }
            let mut methods = Vec::new();
            for row in &self.methods {
                let (name, name_len) = cell(row.name);
                let (receiver, receiver_len) = cell(row.receiver);
                let (blob, blob_len) = cell(row.receiver_params.0);
                methods.extend_from_slice(&row.owner.to_le_bytes());
                methods.extend_from_slice(&[1, 0, 0, 0]);
                methods.extend_from_slice(&name);
                methods.extend_from_slice(&name_len);
                methods.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                methods.extend_from_slice(&receiver);
                methods.extend_from_slice(&receiver_len);
                methods.extend_from_slice(&blob);
                methods.extend_from_slice(&blob_len);
                methods.extend_from_slice(&row.receiver_params.1.to_le_bytes());
                methods.extend_from_slice(&0_u32.to_le_bytes());
                methods.extend_from_slice(&0_u32.to_le_bytes());
                methods.extend_from_slice(&row.span.0.to_le_bytes());
                methods.extend_from_slice(&row.span.1.to_le_bytes());
                methods.extend_from_slice(&file.offset.to_le_bytes());
                methods.extend_from_slice(&file.length.to_le_bytes());
                // Version 6: the authority-bound flag.
                methods.extend_from_slice(&[u8::from(row.bound), 0, 0, 0, 0, 0, 0, 0]);
            }
            let mut parameters = Vec::new();
            for row in &self.parameters {
                let (name, name_len) = cell(row.name);
                parameters.extend_from_slice(&row.owner.to_le_bytes());
                parameters.extend_from_slice(&name);
                parameters.extend_from_slice(&name_len);
                parameters.extend_from_slice(&row.constraint.unwrap_or(NONE).to_le_bytes());
            }
            let mut members = Vec::new();
            for row in &self.members {
                let (name, name_len) = cell(row.name);
                members.extend_from_slice(&row.owner.to_le_bytes());
                members.extend_from_slice(&[row.kind, 0, 1, 0]);
                members.extend_from_slice(&name);
                members.extend_from_slice(&name_len);
                members.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
            }
            let mut docs = Vec::new();
            for row in &self.docs {
                let (text, text_len) = cell(row.text);
                docs.extend_from_slice(&[row.owner_kind, 0, 0, 0]);
                docs.extend_from_slice(&row.owner.to_le_bytes());
                docs.extend_from_slice(&text);
                docs.extend_from_slice(&text_len);
            }
            let mut references = Vec::new();
            for row in &self.references {
                let (target, target_len) = cell(row.target);
                let (package, package_len) = cell(row.target_package);
                let (receiver, receiver_len) = cell(row.receiver);
                let (recv_type, recv_type_len) = cell(row.recv_type);
                references.extend_from_slice(&row.owner.to_le_bytes());
                references.extend_from_slice(&target);
                references.extend_from_slice(&target_len);
                references.extend_from_slice(&package);
                references.extend_from_slice(&package_len);
                references.extend_from_slice(&row.start.to_le_bytes());
                references.extend_from_slice(&row.end.to_le_bytes());
                references.extend_from_slice(&file.offset.to_le_bytes());
                references.extend_from_slice(&file.length.to_le_bytes());
                references.extend_from_slice(&receiver);
                references.extend_from_slice(&receiver_len);
                // Version 6: the closed use kind, the target class, and the
                // receiver type-name atom.
                references.extend_from_slice(&[row.use_kind, row.target_class, 0, 0]);
                references.extend_from_slice(&recv_type);
                references.extend_from_slice(&recv_type_len);
            }
            let mut constraints = Vec::new();
            for row in &self.constraints {
                let (file, file_len) = cell(row.file);
                let (constraint, constraint_len) = cell(row.constraint);
                let (blob, blob_len) = cell(row.blob);
                constraints.extend_from_slice(&file);
                constraints.extend_from_slice(&file_len);
                constraints.extend_from_slice(&constraint);
                constraints.extend_from_slice(&constraint_len);
                constraints.extend_from_slice(&blob);
                constraints.extend_from_slice(&blob_len);
                constraints.extend_from_slice(&row.count.to_le_bytes());
            }
            let mut satisfactions = Vec::new();
            for row in &self.satisfactions {
                let (target, target_len) = cell(row.target);
                let (package, package_len) = cell(row.target_package);
                satisfactions.extend_from_slice(&row.subject.to_le_bytes());
                satisfactions.extend_from_slice(&target);
                satisfactions.extend_from_slice(&target_len);
                satisfactions.extend_from_slice(&package);
                satisfactions.extend_from_slice(&package_len);
            }
            let mut children = Vec::new();
            for target in &self.children {
                children.extend_from_slice(&target.to_le_bytes());
                children.extend_from_slice(&0_u32.to_le_bytes());
            }
            let mut unresolved_cgo = Vec::new();
            for row in &self.unresolved_cgo_cells {
                unresolved_cgo.extend_from_slice(&row.offset.to_le_bytes());
                unresolved_cgo.extend_from_slice(&row.length.to_le_bytes());
            }
            // The fixture has no resolved module metadata.  Keep the module
            // plane absent, as required by its zero header count; packages
            // therefore begin immediately after satisfactions.
            let module = Vec::new();
            let mut packages = Vec::new();
            let import_path = self.atom_cell(PACKAGE);
            let package_name = self.atom_cell(b"demo");
            let files = self.atom_cell(b"main.go\0");
            packages.extend_from_slice(&import_path.offset.to_le_bytes());
            packages.extend_from_slice(&import_path.length.to_le_bytes());
            packages.extend_from_slice(&package_name.offset.to_le_bytes());
            packages.extend_from_slice(&package_name.length.to_le_bytes());
            packages.extend_from_slice(&files.offset.to_le_bytes());
            packages.extend_from_slice(&files.length.to_le_bytes());
            packages.extend_from_slice(&1_u32.to_le_bytes());
            if let Some(extra_path) = self
                .declarations
                .iter()
                .map(|declaration| declaration.package)
                .find(|package| {
                    let package_end = usize::try_from(package.offset).ok().and_then(|offset| {
                        usize::try_from(package.length)
                            .ok()
                            .and_then(|length| offset.checked_add(length))
                    });
                    let import_end = usize::try_from(import_path.offset).ok().and_then(|offset| {
                        usize::try_from(import_path.length)
                            .ok()
                            .and_then(|length| offset.checked_add(length))
                    });
                    match (package_end, import_end) {
                        (Some(package_end), Some(import_end)) => {
                            self.atom_bytes.get(package.offset as usize..package_end)
                                != self.atom_bytes.get(import_path.offset as usize..import_end)
                        }
                        _ => false,
                    }
                })
            {
                let extra_name = self.atom_cell(b"Extra");
                packages.extend_from_slice(&extra_path.offset.to_le_bytes());
                packages.extend_from_slice(&extra_path.length.to_le_bytes());
                packages.extend_from_slice(&extra_name.offset.to_le_bytes());
                packages.extend_from_slice(&extra_name.length.to_le_bytes());
                packages.extend_from_slice(&files.offset.to_le_bytes());
                packages.extend_from_slice(&files.length.to_le_bytes());
                packages.extend_from_slice(&1_u32.to_le_bytes());
            }
            let package_count = u32::try_from(packages.len() / 28).map_err(TestError::from)?;

            let mut signature_parameters = Vec::new();
            for (owner, row) in self.types.iter().enumerate() {
                if row.kind != ROW_FUNC {
                    continue;
                }
                let owner_u32 = u32::try_from(owner).map_err(TestError::from)?;
                for ordinal in 0..row.children.1 {
                    let name = self
                        .signature_parameter_names
                        .iter()
                        .find(|(named_owner, named_ordinal, _)| {
                            *named_owner == owner_u32 && *named_ordinal == ordinal
                        })
                        .map(|(_, _, cell)| *cell)
                        .unwrap_or(Cell {
                            offset: 0,
                            length: 0,
                        });
                    signature_parameters.extend_from_slice(&owner_u32.to_le_bytes());
                    signature_parameters.extend_from_slice(&ordinal.to_le_bytes());
                    signature_parameters.extend_from_slice(&name.offset.to_le_bytes());
                    signature_parameters.extend_from_slice(&name.length.to_le_bytes());
                    signature_parameters.extend_from_slice(&0_u32.to_le_bytes());
                    signature_parameters.extend_from_slice(&0_u32.to_le_bytes());
                    signature_parameters.extend_from_slice(&NONE.to_le_bytes());
                }
            }

            let mut method_sets = Vec::new();
            if self.method_sets.is_empty() {
                for (owner, row) in self.types.iter().enumerate() {
                    if row.kind != ROW_INTERFACE {
                        continue;
                    }
                    let start = usize::try_from(row.members.0).map_err(TestError::from)?;
                    let end = start
                        .checked_add(usize::try_from(row.members.1).map_err(TestError::from)?)
                        .ok_or(TestError::Tail)?;
                    for member in self.members.get(start..end).ok_or(TestError::Tail)? {
                        if member.kind != 1 {
                            continue;
                        }
                        let (name, name_len) = cell(member.name);
                        method_sets.extend_from_slice(
                            &u32::try_from(owner).map_err(TestError::from)?.to_le_bytes(),
                        );
                        method_sets.extend_from_slice(&name);
                        method_sets.extend_from_slice(&name_len);
                        method_sets
                            .extend_from_slice(&member.type_root.unwrap_or(NONE).to_le_bytes());
                        let package = self.atom_cell(PACKAGE);
                        method_sets.extend_from_slice(&package.offset.to_le_bytes());
                        method_sets.extend_from_slice(&package.length.to_le_bytes());
                    }
                }
            } else {
                for row in &self.method_sets {
                    let (name, name_len) = cell(row.name);
                    method_sets.extend_from_slice(&row.owner.to_le_bytes());
                    method_sets.extend_from_slice(&name);
                    method_sets.extend_from_slice(&name_len);
                    method_sets.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                    let package = row.package;
                    method_sets.extend_from_slice(&package.offset.to_le_bytes());
                    method_sets.extend_from_slice(&package.length.to_le_bytes());
                }
            }

            let sections = [
                declarations,
                types,
                methods,
                parameters,
                members,
                docs,
                references,
                constraints,
                satisfactions,
                module,
                packages,
                signature_parameters,
                method_sets,
                children,
                unresolved_cgo,
                self.atom_bytes.clone(),
            ];
            let counts = [
                count(self.declarations.len())?,
                count(self.types.len())?,
                count(self.methods.len())?,
                count(self.parameters.len())?,
                count(self.members.len())?,
                count(self.docs.len())?,
                count(self.references.len())?,
                count(self.constraints.len())?,
                count(self.satisfactions.len())?,
            ];
            let body = sections.iter().map(Vec::len).sum::<usize>();
            let mut image = vec![0_u8; HEADER_BYTES];
            image[..4].copy_from_slice(b"NGAI");
            image[4..6].copy_from_slice(&6_u16.to_le_bytes());
            image[6..8].copy_from_slice(
                &u16::try_from(HEADER_BYTES)
                    .map_err(TestError::from)?
                    .to_le_bytes(),
            );
            image[8..12].copy_from_slice(&counts[0].to_le_bytes());
            image[12..16].copy_from_slice(&count(self.atom_bytes.len())?.to_le_bytes());
            image[16..20].copy_from_slice(&count(body)?.to_le_bytes());
            image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
            image[84..88].copy_from_slice(&counts[1].to_le_bytes());
            image[88..92].copy_from_slice(&counts[6].to_le_bytes());
            image[92..96].copy_from_slice(&counts[2].to_le_bytes());
            image[96..100].copy_from_slice(&counts[3].to_le_bytes());
            image[100..104].copy_from_slice(&counts[4].to_le_bytes());
            image[104..108].copy_from_slice(&counts[5].to_le_bytes());
            image[108..112].copy_from_slice(&counts[7].to_le_bytes());
            image[112..116].copy_from_slice(&counts[8].to_le_bytes());
            image[116..120].copy_from_slice(&0_u32.to_le_bytes());
            image[120..124].copy_from_slice(&package_count.to_le_bytes());
            image[124..128].copy_from_slice(&count(sections[11].len() / 28)?.to_le_bytes());
            image[128..132].copy_from_slice(&count(sections[12].len() / 24)?.to_le_bytes());
            image[132..136].copy_from_slice(&count(self.unresolved_cgo_cells.len())?.to_le_bytes());
            let mut digest = Sha256::new();
            digest.update(IMAGE_DOMAIN);
            digest.update(&image[..52]);
            digest.update(&image[84..HEADER_BYTES]);
            for section in &sections {
                digest.update(section);
            }
            image[52..84].copy_from_slice(&digest.finalize());
            let mut complete = image;
            for section in &sections {
                complete.extend_from_slice(section);
            }
            Ok(complete)
        }
    }

    /// Lows one fixture image and writes the committed fragment, proving the
    /// untouched output tail stayed unchanged.
    fn lower(fix: &Fixture, source: &[u8]) -> Result<Vec<u8>, TestError> {
        let image = fix.encode(source)?;
        let mut facts = FactSet::new();
        collect(source, &image, &mut facts)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(TestError::from)?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
            Stage::LowerIr,
            NativeTool::GoCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"go-authority-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length =
            crate::driver::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
                .map_err(TestError::from)?
                .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Materializes the owned semantic image for one fixture image — the
    /// surface where the link plane and its absolute, source-verified
    /// occurrence sites live.
    fn lower_ir(fix: &Fixture, source: &[u8]) -> Result<backend_semantic::ir::Ir, TestError> {
        let image = fix.encode(source)?;
        let mut facts = FactSet::new();
        collect(source, &image, &mut facts)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(TestError::from)?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
            Stage::LowerIr,
            NativeTool::GoCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"go-authority-toolchain"),
        );
        facts
            .build_ir(
                LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
                identity,
                recipe,
                crate::driver::types::DeclarationScope::fixture(),
            )
            .map_err(TestError::Ir)
    }

    /// The byte extent of the first occurrence of one needle in the test
    /// source, so fixture spans can be proven against the exact bytes they
    /// claim to cover.
    fn at(source: &[u8], needle: &[u8]) -> Result<(u32, u32), TestError> {
        let at = source
            .windows(needle.len())
            .position(|window| window == needle)
            .ok_or(TestError::Missing("fixture needle in source"))?;
        Ok((
            u32::try_from(at).map_err(TestError::from)?,
            u32::try_from(at + needle.len()).map_err(TestError::from)?,
        ))
    }

    /// Borrows one validated type-fact row by its wire ordinal.
    fn row<'fragment>(
        view: &FragmentView<'fragment>,
        ordinal: usize,
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        cursor
            .nth(ordinal)
            .ok_or(TestError::Missing("type row"))?
            .map_err(TestError::from)
    }

    /// Finds one canonical entity by name. Entity order is independent of
    /// both source declaration order and canonical type-row order.
    fn entity_of(view: &FragmentView<'_>, name: &[u8]) -> Result<EntityId, TestError> {
        for entity in view.entities() {
            let atom = view
                .atoms()
                .nth(usize::try_from(entity.name.raw).map_err(TestError::from)?)
                .ok_or(TestError::Missing("entity atom"))?;
            if atom.bytes == name {
                return Ok(entity.entity);
            }
        }
        Err(TestError::Missing("entity by name"))
    }

    /// The canonical kind of one named entity row.
    fn entity_kind_of(view: &FragmentView<'_>, name: &[u8]) -> Result<EntityKind, TestError> {
        for entity in view.entities() {
            let atom = view
                .atoms()
                .nth(usize::try_from(entity.name.raw).map_err(TestError::from)?)
                .ok_or(TestError::Missing("entity atom"))?;
            if atom.bytes == name {
                return Ok(entity.kind);
            }
        }
        Err(TestError::Missing("entity by name"))
    }

    /// Finds the declared type row owned by one canonical entity.
    fn row_for_entity<'fragment>(
        view: &'fragment FragmentView<'fragment>,
        entity: EntityId,
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        view.type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|row| row.owner == entity)
            .next_back()
            .ok_or(TestError::Missing("entity type row"))
    }

    fn row_for_name<'fragment>(
        view: &'fragment FragmentView<'fragment>,
        name: &[u8],
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        row_for_entity(view, entity_of(view, name)?)
    }

    /// Borrows the typed Go extension bound to one canonical entity.
    fn go_extension(
        view: &FragmentView<'_>,
        ordinal: usize,
    ) -> Result<backend_semantic::ir::GoFacts, TestError> {
        view.discover()
            .language_extensions()
            .map_err(|_| TestError::Missing("extension section"))?
            .ok_or(TestError::Missing("extension section"))?
            .go
            .get(EntityId::new(
                u32::try_from(ordinal).map_err(TestError::from)?,
            ))
            .map_err(|_| TestError::Missing("go extension row"))?
            .ok_or(TestError::Missing("go extension row"))
    }

    /// Lends one pooled reference list through its closed wire lane rather
    /// than advancing over type-parameter rows with a historical byte stride.
    fn pooled_list<'a>(
        view: &'a FragmentView<'a>,
        lane: backend_semantic::ir::ExtensionPoolListLane,
        ordinal: u32,
    ) -> Result<Vec<u32>, TestError> {
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Tail)?
            .ok_or(TestError::Missing("pool payload"))?;
        Ok(pools
            .reference_list(lane, ordinal)
            .map_err(|_| TestError::Missing("pooled list"))?
            .iter()
            .collect())
    }

    #[test]
    fn empty_image_admits_the_current_schema_fragment_without_semantic_sections()
    -> Result<(), TestError> {
        let fix = Fixture::new();
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some() || view.occurrences().is_some() || view.docs().is_some() {
            return Err(TestError::Missing("absent semantic sections"));
        }
        Ok(())
    }

    #[test]
    fn one_named_type_commits_its_diagonal_self_nominal() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Node", None);
        let bytes = lower(&fix, b"package demo\ntype Node struct{}\n")?;
        let view = FragmentView::validate(&bytes)?;
        let only = row(&view, 0)?;
        if only.owner.raw != 0
            || only.record.tag != SemanticTypeTag::Nominal
            || only.record.nominal != Some(NominalRef::Local(EntityId::new(0)))
        {
            return Err(TestError::Missing("diagonal self nominal"));
        }
        if row(&view, 1).is_ok() {
            return Err(TestError::Missing("single row"));
        }
        Ok(())
    }

    /// The kind selection of pass-one named types follows the underlying
    /// shape: an interface root row projects the canonical trait, a struct
    /// root row stays a record, and a true alias stays an alias.
    #[test]
    fn interface_types_project_traits_while_structs_and_aliases_keep_their_kinds()
    -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"next", None);
        fix.declarations[node].type_root = Some(struct_row);
        let closer = fix.declaration(KIND_TYPE, b"Closer", None);
        let iface_row = fix.start_row(ROW_INTERFACE);
        fix.interface_method(iface_row, b"Close", None);
        fix.declarations[closer].type_root = Some(iface_row);
        let int = fix.basic(b"int");
        fix.declaration(KIND_ALIAS, b"Count", Some(int));
        let source = b"package demo\n";
        // Two-run byte stability: the kind reframe must not make the
        // fragment digest input-order or memory dependent.
        let first = lower(&fix, source)?;
        let second = lower(&fix, source)?;
        if first != second {
            return Err(TestError::Tail);
        }
        let bytes = first;
        let view = FragmentView::validate(&bytes)?;
        let expected: [(&[u8], EntityKind); 3] = [
            (b"Node", EntityKind::Record),
            (b"Closer", EntityKind::Trait),
            (b"Count", EntityKind::Alias),
        ];
        for (name, kind) in expected {
            if entity_kind_of(&view, name)? != kind {
                return Err(TestError::Missing("kind selection"));
            }
        }
        Ok(())
    }

    #[test]
    fn recursive_pointer_field_targets_the_backward_nominal() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let int = fix.basic(b"int");
        let _ = int;
        let named = fix.named(PACKAGE, b"Node", &[]);
        let pointer = fix.unary(ROW_POINTER, named);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"next", Some(pointer));
        fix.declarations[node].type_root = Some(struct_row);
        let bytes = lower(&fix, b"package demo\ntype Node struct{ next *Node }\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Node, 1 next. The pointer child names fact 0 directly.
        let field = row(&view, 1)?;
        let children = field_children(&view, &field)?;
        if field.record.tag != SemanticTypeTag::Primitive
            || field.record.payload0 != SHAPE_MUT_POINTER
            || children != vec![0]
        {
            return Err(TestError::Missing("backward pointer field"));
        }
        // Falsifier: retyping the field to a foreign named row folds to the
        // typed unknown that retains the spelling.
        let mut mutated = fix.clone();
        let foreign = mutated.named(b"example.com/other", b"Other", &[]);
        let member_index = mutated
            .members
            .iter()
            .position(|member| member.name.length == 4)
            .ok_or(TestError::Missing("member row"))?;
        mutated.members[member_index].type_root = Some(foreign);
        let other = lower(&mutated, b"package demo\ntype Node struct{ next *Node }\n")?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let field = row(&view, 1)?;
        if field.record.tag != SemanticTypeTag::Unknown
            || field.record.text != Some(b"Other".as_slice())
        {
            return Err(TestError::Missing("foreign field fold"));
        }
        Ok(())
    }

    /// Borrows local child coordinates from the validated typed child cursor.
    fn field_children<'fragment>(
        view: &FragmentView<'fragment>,
        fact: &backend_semantic::ir::DecodedTypeFact<'fragment>,
    ) -> Result<Vec<u32>, TestError> {
        let start = fact.record.children.start;
        let end = start
            .checked_add(fact.record.children.length)
            .ok_or(TestError::Tail)?;
        let mut positions = Vec::new();
        for child in view
            .type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .children()?
        {
            let child = child?;
            if child.ordinal < start || child.ordinal >= end {
                continue;
            }
            let backend_semantic::ir::TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                target,
            )) = child.child.target
            else {
                return Err(TestError::Missing("local child"));
            };
            positions.push(target.raw);
        }
        Ok(positions)
    }

    #[test]
    fn primitives_commit_exact_width_signedness_and_builtin_cells() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let spellings: [(&[u8], &[u8]); 9] = [
            (b"A", b"int"),
            (b"B", b"uint8"),
            (b"C", b"byte"),
            (b"D", b"rune"),
            (b"E", b"float32"),
            (b"F", b"float64"),
            (b"G", b"complex64"),
            (b"H", b"bool"),
            (b"I", b"string"),
        ];
        for (name, spelling) in spellings {
            let basic = fix.basic(spelling);
            fix.declaration(KIND_VAR, name, Some(basic));
        }
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let expected: [(u32, u32, u32); 9] = [
            (SHAPE_NATIVE_SIGNED_INTEGER, 0, 0),
            (SHAPE_INTEGER, 8 << INTEGER_WIDTH_SHIFT, 0),
            (SHAPE_INTEGER, 8 << INTEGER_WIDTH_SHIFT, 0),
            (SHAPE_INTEGER, (32 << INTEGER_WIDTH_SHIFT) | 1, 0),
            (SHAPE_FLOAT, TypeWidth::Fixed(32).to_cell(), 0),
            (SHAPE_FLOAT, TypeWidth::Fixed(64).to_cell(), 0),
            (SHAPE_BUILTIN, 0, 0),
            (SHAPE_BOOL, 0, 0),
            (SHAPE_STR, 0, 0),
        ];
        for (index, (shape, payload1, _)) in expected.iter().enumerate() {
            let fact = row(&view, index)?;
            if fact.record.tag != SemanticTypeTag::Primitive
                || fact.record.payload0 != *shape
                || fact.record.payload1 != *payload1
            {
                return Err(TestError::Missing("primitive cells"));
            }
        }
        let complex_row = row(&view, 6)?;
        if complex_row.record.text != Some(b"complex64".as_slice()) {
            return Err(TestError::Missing("complex builtin spelling"));
        }
        Ok(())
    }

    #[test]
    fn slices_arrays_maps_and_channels_project_their_shapes() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let boolean = fix.basic(b"bool");
        let slice = fix.unary(ROW_SLICE, int);
        let pointer = fix.unary(ROW_POINTER, int);
        let array = fix.array(3, int);
        let map = fix.map(string, boolean);
        let channel = fix.chan(1, int);
        fix.declaration(KIND_VAR, b"S", Some(slice));
        fix.declaration(KIND_VAR, b"P", Some(pointer));
        fix.declaration(KIND_VAR, b"Arr", Some(array));
        fix.declaration(KIND_VAR, b"M", Some(map));
        fix.declaration(KIND_VAR, b"Ch", Some(channel));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let slice_row = row_for_name(&view, b"S")?;
        let slice_children = field_children(&view, &slice_row)?;
        if slice_row.record.tag != SemanticTypeTag::Slice
            || slice_children.len() != 1
            || row(&view, slice_children[0] as usize)?.record.tag != SemanticTypeTag::Primitive
        {
            return Err(TestError::Missing("slice row"));
        }
        let pointer_row = row_for_name(&view, b"P")?;
        let pointer_children = field_children(&view, &pointer_row)?;
        if pointer_row.record.payload0 != SHAPE_MUT_POINTER
            || pointer_children.len() != 1
            || row(&view, pointer_children[0] as usize)?.record.tag != SemanticTypeTag::Primitive
        {
            return Err(TestError::Missing("pointer row"));
        }
        let array_row = row_for_name(&view, b"Arr")?;
        if array_row.record.tag != SemanticTypeTag::ArrayFixed
            || array_row.record.payload0 != 3
            || array_row.record.payload1 != 0
        {
            return Err(TestError::Missing("array length cells"));
        }
        let map_row = row_for_name(&view, b"M")?;
        let map_children = field_children(&view, &map_row)?;
        if map_row.record.tag != SemanticTypeTag::Map
            || map_children.len() != 2
            || row(&view, map_children[0] as usize)?.record.payload0 != SHAPE_STR
            || row(&view, map_children[1] as usize)?.record.payload0 != SHAPE_BOOL
        {
            return Err(TestError::Missing("structural map"));
        }
        let chan_row = row_for_name(&view, b"Ch")?;
        let channel_children = field_children(&view, &chan_row)?;
        if chan_row.record.tag != SemanticTypeTag::Channel
            || chan_row.record.payload0 != ChannelDirection::Send as u32
            || channel_children.len() != 1
            || row(&view, channel_children[0] as usize)?.record.tag != SemanticTypeTag::Primitive
        {
            return Err(TestError::Missing("directional channel"));
        }
        Ok(())
    }

    #[test]
    fn signatures_commit_carriers_results_and_variadic_flag() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let error = fix.named(b"", b"error", &[]);
        let brew = fix.func(&[int, string], &[int, error], false);
        fix.declaration(KIND_FUNC, b"Brew", Some(brew));
        let slice = fix.unary(ROW_SLICE, int);
        let variadic = fix.func(&[slice], &[], true);
        fix.declaration(KIND_FUNC, b"V", Some(variadic));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 int carrier, 1 string carrier, 2 int result carrier,
        // 3 error result carrier, 4 Brew, 5 xs carrier, 6 V.
        for ordinal in 0..4 {
            let carrier = row(&view, ordinal)?;
            if carrier.record.tag == SemanticTypeTag::FunctionPointer {
                return Err(TestError::Missing("carrier row"));
            }
        }
        let error_carrier = row(&view, 4)?;
        if error_carrier.record.tag != SemanticTypeTag::Unknown
            || error_carrier.record.text != Some(b"error".as_slice())
        {
            return Err(TestError::Missing("universe error fold"));
        }
        let brew = row(&view, 5)?;
        if brew.record.tag != SemanticTypeTag::FunctionPointer
            || brew.record.payload1 != 2
            || field_children(&view, &brew)? != vec![1, 2, 3, 4]
        {
            return Err(TestError::Missing("brew function pointer"));
        }
        let brew_facts = go_extension(&view, 4)?;
        if brew_facts.signature.parameters.raw != 1
            || brew_facts.signature.results.raw != 2
            || brew_facts.signature.variadic
        {
            return Err(TestError::Missing("brew signature lists"));
        }
        let variadic_row = row(&view, 7)?;
        if variadic_row.record.payload1 != 0
            || variadic_row.record.payload0 != SemanticTypeRecord::FUNCTION_TYPED_VARIADIC_FLAG
        {
            return Err(TestError::Missing("void variadic result flag"));
        }
        let variadic_facts = go_extension(&view, 6)?;
        if !variadic_facts.signature.variadic || variadic_facts.signature.results.raw != 0 {
            return Err(TestError::Missing("variadic signature fact"));
        }
        Ok(())
    }

    #[test]
    fn struct_fields_and_interface_methods_join_the_go_extension() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let error = fix.named(b"", b"error", &[]);
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let node_struct = fix.start_row(ROW_STRUCT);
        fix.field(node_struct, b"Name", Some(string));
        fix.field(node_struct, b"Size", Some(int));
        fix.declarations[node].type_root = Some(node_struct);
        let get = fix.func(&[], &[int], false);
        fix.method(node, b"Get", Some(get));
        let iface = fix.declaration(KIND_TYPE, b"Store", None);
        let iface_row = fix.start_row(ROW_INTERFACE);
        let put = fix.func(&[int], &[error], false);
        fix.interface_method(iface_row, b"Put", Some(put));
        fix.declarations[iface].type_root = Some(iface_row);
        let empty = fix.declaration(KIND_TYPE, b"Bag", None);
        let empty_row = fix.start_row(ROW_INTERFACE);
        fix.declarations[empty].type_root = Some(empty_row);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Node, 1 Store, 2 Bag, 3 Name, 4 Size, 5 int result, 6 Get,
        // 7 int param, 8 error result, 9 Put.
        let node_facts = go_extension(&view, 0)?;
        if node_facts.fields.raw != 1
            || pooled_list(
                &view,
                backend_semantic::ir::ExtensionPoolListLane::Entities,
                1,
            )? != vec![3, 4]
        {
            return Err(TestError::Missing("node field list"));
        }
        if node_facts.method_set.raw != 2
            || pooled_list(
                &view,
                backend_semantic::ir::ExtensionPoolListLane::Entities,
                2,
            )? != vec![6]
        {
            return Err(TestError::Missing("node method set"));
        }
        let store_facts = go_extension(&view, 1)?;
        if store_facts.method_set.raw != 3
            || pooled_list(
                &view,
                backend_semantic::ir::ExtensionPoolListLane::Entities,
                3,
            )? != vec![9]
        {
            return Err(TestError::Missing("store method set"));
        }
        let mut with_plane_row = fix.clone();
        with_plane_row.method_set(iface_row, b"Put", Some(put));
        if lower(&with_plane_row, b"package demo\n")? != bytes {
            return Err(TestError::Missing("method-set duplicate"));
        }
        let get_row = row(&view, 6)?;
        if get_row.record.tag != SemanticTypeTag::FunctionPointer
            || get_row.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || field_children(&view, &get_row)? != vec![5]
        {
            return Err(TestError::Missing("get signature row"));
        }
        let put_row = row(&view, 9)?;
        if put_row.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || field_children(&view, &put_row)? != vec![7, 8]
        {
            return Err(TestError::Missing("put signature row"));
        }
        let bag_facts = go_extension(&view, 2)?;
        if bag_facts.fields.raw != 0 || bag_facts.method_set.raw != 0 {
            return Err(TestError::Missing("empty bag extension"));
        }
        Ok(())
    }

    #[test]
    fn method_set_bound_covers_seventeen_and_rejects_thirty_three() -> Result<(), TestError> {
        let fixture = |count: usize| {
            let mut fix = Fixture::new();
            let owner = fix.declaration(KIND_TYPE, b"Authority", None);
            let authority = fix.start_row(ROW_INTERFACE);
            fix.declarations[owner].type_root = Some(authority);
            for index in 0..count {
                let name = format!("M{index:03}");
                fix.method_set(authority, name.as_bytes(), None);
            }
            fix
        };

        for count in 33..=MAX_REF_LIST_ELEMENTS {
            let legal = fixture(count);
            let bytes = lower(&legal, b"package demo\ntype Authority struct{}\n")?;
            let view = FragmentView::validate(&bytes)?;
            let facts = go_extension(&view, entity_of(&view, b"Authority")?.index())?;
            if pooled_list(
                &view,
                backend_semantic::ir::ExtensionPoolListLane::Entities,
                facts.method_set.raw,
            )?
            .len()
                != count
            {
                return Err(TestError::Missing("method-set declarations through width"));
            }
        }

        let mut foreign = Fixture::new();
        let box_decl = foreign.declaration(KIND_TYPE, b"Box", None);
        let box_row = foreign.start_row(ROW_INTERFACE);
        foreign.declarations[box_decl].type_root = Some(box_row);
        let read_sig = foreign.func(&[], &[], false);
        let error_sig = foreign.func(&[], &[], false);
        foreign.method_set_from(box_row, b"Error", Some(error_sig), b"");
        foreign.method_set_from(box_row, b"Read", Some(read_sig), b"io");
        let foreign_bytes = lower(&foreign, b"package demo\n")?;
        let foreign_view = FragmentView::validate(&foreign_bytes)?;
        let box_entity = entity_of(&foreign_view, b"Box")?;
        let box_facts = go_extension(&foreign_view, box_entity.index())?;
        let promoted = pooled_list(
            &foreign_view,
            backend_semantic::ir::ExtensionPoolListLane::Entities,
            box_facts.method_set.raw,
        )?;
        if promoted.len() != 2 {
            return Err(TestError::Missing("promoted method set width"));
        }
        let mut promoted_names = Vec::new();
        for ordinal in promoted {
            let mut found = None;
            for entity in foreign_view.entities() {
                if entity.entity.index() == usize::try_from(ordinal).map_err(TestError::from)? {
                    let atom = foreign_view
                        .atoms()
                        .nth(usize::try_from(entity.name.raw).map_err(TestError::from)?)
                        .ok_or(TestError::Missing("promoted atom"))?;
                    if entity.kind != EntityKind::Function {
                        return Err(TestError::Missing("promoted method kind"));
                    }
                    found = Some(atom.bytes.to_vec());
                }
            }
            promoted_names.push(found.ok_or(TestError::Missing("promoted entity"))?);
        }
        if promoted_names != [b"Error".to_vec(), b"Read".to_vec()] {
            return Err(TestError::Missing("foreign and universe method names"));
        }
        let ir = lower_ir(&foreign, b"package demo\n")?;
        let box_id = ir
            .items()
            .find(|item| item.name() == b"Box")
            .ok_or(TestError::Missing("box item"))?
            .id();
        for name in [b"Error".as_slice(), b"Read".as_slice()] {
            let method = ir
                .items()
                .find(|item| item.name() == name)
                .ok_or(TestError::Missing("promoted item"))?;
            if method.parent() != Some(box_id) {
                return Err(TestError::Missing("promoted method parent"));
            }
        }

        let beyond_width = fixture(MAX_REF_LIST_ELEMENTS + 1);
        match lower(&beyond_width, b"package demo\ntype Authority struct{}\n") {
            Err(TestError::Collect(GoCollectError::Lowering(
                LoweringUnsupported::GoProjection {
                    fault:
                        PortableGoProjectionFault::ListCapacity {
                            owner: 0,
                            phase: GoProjectionListPhase::Entity,
                        },
                },
            ))) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(TestError::Missing("method-set capacity rejection")),
        }
    }

    #[test]
    fn generic_named_roots_apply_base_and_arguments() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let pair = fix.declaration(KIND_TYPE, b"Pair", None);
        let pair_struct = fix.start_row(ROW_STRUCT);
        let param = fix.type_param_row(b"K");
        fix.field(pair_struct, b"Head", Some(param));
        fix.declarations[pair].type_root = Some(pair_struct);
        fix.type_parameter(pair, b"K", None);
        let applied = fix.named(PACKAGE, b"Pair", &[int]);
        fix.declaration(KIND_VAR, b"P", Some(applied));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Wire rows: 0 int (anonymous), 1 Pair, 2 Head (TypeVar K), 3 P.
        let head = row(&view, 2)?;
        if head.record.tag != SemanticTypeTag::TypeVar || head.record.text != Some(b"K".as_slice())
        {
            return Err(TestError::Missing("type parameter reference"));
        }
        let applied_row = row(&view, 3)?;
        if applied_row.record.tag != SemanticTypeTag::Apply
            || field_children(&view, &applied_row)? != vec![1, 0]
        {
            return Err(TestError::Missing("pair application children"));
        }
        let parameters = pooled_type_parameters(&view)?;
        if parameters.len() != 1 || parameters[0].0 != b"K".as_slice() || parameters[0].1.is_some()
        {
            return Err(TestError::Missing("pooled parameter row"));
        }
        Ok(())
    }

    #[test]
    fn named_type_resolution_uses_each_declarations_package() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"First", None);
        let _extra_name = fix.atom(b"Extra");
        let second_package = fix.atom(b"example.com/demo/second");
        let second = fix.declaration(KIND_TYPE, b"Second", None);
        fix.declarations[second].package = second_package;
        let reference = fix.named(b"example.com/demo/second", b"Second", &[]);
        let value = fix.declaration(KIND_VAR, b"Value", Some(reference));
        fix.declarations[value].package = second_package;

        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let projected = row_for_name(&view, b"Value")?;
        let second = entity_of(&view, b"Second")?;
        if projected.record.tag != SemanticTypeTag::Nominal
            || projected.record.nominal != Some(NominalRef::Local(second))
        {
            return Err(TestError::Missing("second-package local nominal"));
        }
        Ok(())
    }

    #[test]
    fn pending_compound_rows_bind_to_the_fact_pushed_next() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Anchor", None);
        let int = fix.basic(b"int");
        let slice = fix.unary(ROW_SLICE, int);
        let map = fix.map(int, slice);
        let value = fix.declaration(KIND_VAR, b"value", Some(map));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut rows = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        let slice = rows
            .find_map(|row| match row {
                Ok(row) if row.record.tag == SemanticTypeTag::Slice => Some(Ok(row)),
                Ok(_) => None,
                Err(error) => Some(Err(TestError::from(error))),
            })
            .transpose()?
            .ok_or(TestError::Missing("pending slice row"))?;
        if slice.owner.raw != u32::try_from(value).map_err(TestError::from)? {
            return Err(TestError::Missing("pending compound owner"));
        }
        Ok(())
    }

    #[test]
    fn shared_nonleaf_anonymous_type_is_reinterned_for_each_pending_owner() -> Result<(), TestError>
    {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Key", None);
        let key = fix.named(PACKAGE, b"Key", &[]);
        let int = fix.basic(b"int");
        let slice = fix.unary(ROW_SLICE, int);
        let first = fix.map(key, slice);
        let second = fix.map(key, slice);
        let first_owner = fix.declaration(KIND_VAR, b"first", Some(first));
        let second_owner = fix.declaration(KIND_VAR, b"second", Some(second));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let owners = view
            .type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .filter_map(|row| match row {
                Ok(row) if row.record.tag == SemanticTypeTag::Slice => Some(Ok(row.owner.raw)),
                Ok(_) => None,
                Err(error) => Some(Err(TestError::from(error))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let first_owner = u32::try_from(first_owner).map_err(TestError::from)?;
        let second_owner = u32::try_from(second_owner).map_err(TestError::from)?;
        if owners.len() != 2
            || !owners.contains(&first_owner)
            || !owners.contains(&second_owner)
            || first_owner == second_owner
        {
            return Err(TestError::Missing("owner-qualified shared anonymous rows"));
        }
        Ok(())
    }

    /// Decodes every pooled type parameter's first type bound through the
    /// schema-aware borrowed pool view, never by assuming a byte stride.
    fn pooled_type_parameters<'a>(
        view: &'a FragmentView<'a>,
    ) -> Result<Vec<(&'a [u8], Option<u32>)>, TestError> {
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Tail)?
            .ok_or(TestError::Missing("pool payload"))?;
        let mut parameters = Vec::new();
        for ordinal in 0..pools.type_parameter_count() {
            let parameter = pools.type_parameter(ordinal).map_err(|_| TestError::Tail)?;
            let constraint = match parameter.semantics {
                backend_semantic::ir::DecodedTypeParameterSemantics::Exact { .. } => pools
                    .type_parameter_bounds(parameter)
                    .map_err(|_| TestError::Tail)?
                    .and_then(|bounds| bounds.get(0).ok())
                    .and_then(|bound| match bound {
                        backend_semantic::ir::DecodedTypeParameterBound::Type(raw) => Some(raw),
                        backend_semantic::ir::DecodedTypeParameterBound::Lifetime(_) => None,
                    }),
                backend_semantic::ir::DecodedTypeParameterSemantics::Legacy { constraint } => {
                    constraint
                }
            };
            parameters.push((parameter.name, constraint));
        }
        Ok(parameters)
    }

    #[test]
    fn nested_named_references_inside_anonymous_rows_fold_with_spelling() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Node", None);
        let named = fix.named(PACKAGE, b"Node", &[]);
        let slice = fix.unary(ROW_SLICE, named);
        let channel = fix.chan(0, slice);
        fix.declaration(KIND_VAR, b"W", Some(channel));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let channel_row = row_for_name(&view, b"W")?;
        let channel_children = field_children(&view, &channel_row)?;
        let slice_row = channel_children
            .first()
            .map(|target| row(&view, *target as usize))
            .transpose()?
            .ok_or(TestError::Missing("channel over slice"))?;
        if channel_row.record.tag != SemanticTypeTag::Channel
            || slice_row.record.tag != SemanticTypeTag::Slice
        {
            return Err(TestError::Missing("channel over slice"));
        }
        let folded = row_by_payload_text(&view, b"Node")?;
        if folded.record.tag != SemanticTypeTag::Unknown
            || folded.record.payload0 != TypeReason::NoIrRepresentation as u32
            || folded.record.text != Some(b"Node".as_slice())
        {
            return Err(TestError::Missing("anonymous named fold"));
        }
        Ok(())
    }

    /// Scans the type-fact payload for the row at the given anonymous wire
    /// ordinal carrying the given text cell.
    fn row_by_payload_text<'fragment>(
        view: &FragmentView<'fragment>,
        text: &[u8],
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        while let Some(fact) = cursor.next() {
            let fact = fact.map_err(TestError::from)?;
            if fact.record.text == Some(text) {
                return Ok(fact);
            }
        }
        Err(TestError::Missing("folded row"))
    }

    #[test]
    fn aliases_project_their_target_records() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let b_decl = fix.declaration(KIND_TYPE, b"B", None);
        let named = fix.named(PACKAGE, b"B", &[]);
        let a_decl = fix.declaration(KIND_ALIAS, b"A", Some(named));
        let _ = a_decl;
        let c_decl = fix.declaration(KIND_ALIAS, b"C", Some(int));
        let _ = c_decl;
        let _ = b_decl;
        fix.doc(
            DOC_DECLARATION,
            u32::try_from(a_decl).map_err(TestError::from)?,
            b"Alias documentation",
        );
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let alias_entity = entity_of(&view, b"A")?;
        let target_entity = entity_of(&view, b"B")?;
        let primitive_entity = entity_of(&view, b"C")?;
        let alias = row_for_entity(&view, alias_entity)?;
        if alias.record.tag != SemanticTypeTag::Nominal
            || alias.record.nominal != Some(NominalRef::Local(target_entity))
        {
            return Err(TestError::Missing("alias nominal target"));
        }
        let primitive = row_for_entity(&view, primitive_entity)?;
        if primitive.record.tag != SemanticTypeTag::Primitive
            || primitive.record.payload0 != SHAPE_NATIVE_SIGNED_INTEGER
        {
            return Err(TestError::Missing("alias primitive target"));
        }
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let doc = docs.next().ok_or(TestError::Missing("alias doc"))??;
        if doc.owner != alias_entity
            || doc.fragment != DocFragmentInput::Text(b"Alias documentation")
        {
            return Err(TestError::Missing("alias doc owner"));
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact alias docs"));
        }
        Ok(())
    }

    #[test]
    fn empty_interface_values_commit_the_any_leaf() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let iface = fix.start_row(ROW_INTERFACE);
        fix.declaration(KIND_VAR, b"X", Some(iface));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let any_row = row(&view, 0)?;
        if any_row.record.tag != SemanticTypeTag::Any {
            return Err(TestError::Missing("any leaf"));
        }
        Ok(())
    }

    #[test]
    fn docs_become_text_and_softbreak_fragments() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let named = fix.named(PACKAGE, b"Node", &[]);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"next", Some(named));
        fix.declarations[node].type_root = Some(struct_row);
        fix.doc(
            DOC_DECLARATION,
            u32::try_from(node).map_err(TestError::from)?,
            b"First line.\nSecond line.",
        );
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let expected: [DocFragmentInput<'_>; 3] = [
            DocFragmentInput::Text(b"First line."),
            DocFragmentInput::SoftBreak,
            DocFragmentInput::Text(b"Second line."),
        ];
        for fragment in expected {
            let fact = docs.next().ok_or(TestError::Missing("doc fact"))??;
            if fact.owner.raw != 0 || fact.fragment != fragment {
                return Err(TestError::Missing("doc fragment"));
            }
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact doc facts"));
        }
        Ok(())
    }

    #[test]
    fn calls_resolve_local_and_foreign_go_lineage_targets() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let brew = fix.declaration(KIND_FUNC, b"Brew", None);
        let pour = fix.declaration(KIND_FUNC, b"Pour", None);
        let _ = pour;
        let node = fix.declaration(KIND_TYPE, b"T", None);
        let get = fix.method(node, b"Get", None);
        let _ = get;
        fix.reference(
            u32::try_from(brew).map_err(TestError::from)?,
            b"",
            b"Pour",
            b"",
            32,
            36,
        );
        fix.reference(
            u32::try_from(node).map_err(TestError::from)?,
            b"t",
            b"Pour",
            b"",
            48,
            52,
        );
        fix.reference(
            u32::try_from(brew).map_err(TestError::from)?,
            b"",
            b"Print",
            b"fmt",
            64,
            69,
        );
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 T (pass one), 1 Brew, 2 Pour, 3 Get.
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let local = occurrences
            .next()
            .ok_or(TestError::Missing("local call"))??;
        if local.owner.raw != 1
            || local.occurrence.target != OccurrenceTarget::Local(EntityId::new(2))
            || local.occurrence.confidence != OccurrenceConfidence::Oracle
            || local.occurrence.span.start != 32
            || local.occurrence.span.end != 36
        {
            return Err(TestError::Missing("local call fact"));
        }
        let method_call = occurrences
            .next()
            .ok_or(TestError::Missing("method call"))??;
        if method_call.owner.raw != 3 {
            return Err(TestError::Missing("method owner ordinal"));
        }
        let foreign = occurrences
            .next()
            .ok_or(TestError::Missing("foreign call"))??;
        let OccurrenceTarget::Foreign(key) = foreign.occurrence.target else {
            return Err(TestError::Missing("foreign target"));
        };
        let ForeignOrigin::Package(lineage) = key.origin else {
            return Err(TestError::Missing("package origin"));
        };
        if lineage.ecosystem != ECOSYSTEM || lineage.name != "fmt" || key.path != "Print" {
            return Err(TestError::Missing("go fmt lineage"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("exact occurrences"));
        }
        Ok(())
    }

    #[test]
    fn constrained_declarations_carry_their_constraint_atoms() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.constraint(b"windows.go", b"windows", &[(KIND_TYPE, b"WinType")]);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let facts = go_extension(&view, 0)?;
        if facts.build_constraints.raw != 1 {
            return Err(TestError::Missing("constraint atom list"));
        }
        let listed = pooled_list(&view, backend_semantic::ir::ExtensionPoolListLane::Atoms, 1)?;
        if listed != vec![1] {
            return Err(TestError::Missing("constraint atom coordinate"));
        }
        Ok(())
    }

    /// Same-typed blanks share a structural variant. Only the positional
    /// discriminator keeps them from collapsing to one identity.
    fn distinct_identities(
        ir: &backend_semantic::ir::Ir,
        kind: EntityKind,
        name: &[u8],
    ) -> Result<(), TestError> {
        let mut identities = Vec::new();
        for item in ir.items() {
            if item.kind() == kind && item.name() == name {
                identities.push(item.version().identity());
            }
        }
        if identities.len() != 2 {
            return Err(TestError::Missing("two same-typed blank declarations"));
        }
        if identities[0] == identities[1] {
            return Err(TestError::Missing("blank declarations must not share one identity"));
        }
        Ok(())
    }

    #[test]
    fn repeated_blank_struct_fields_do_not_share_one_identity() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let pad = fix.declaration(KIND_TYPE, b"Pad", None);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"_", Some(int));
        fix.field(struct_row, b"Keep", Some(int));
        fix.field(struct_row, b"_", Some(int));
        fix.declarations[pad].type_root = Some(struct_row);
        let ir = lower_ir(&fix, b"package pad\n")?;
        distinct_identities(&ir, EntityKind::Field, b"_")?;
        let mut keep = 0usize;
        for item in ir.items() {
            if item.kind() == EntityKind::Field && item.name() == b"Keep" {
                keep += 1;
            }
        }
        if keep != 1 {
            return Err(TestError::Missing("named field stays one declaration"));
        }
        Ok(())
    }

    #[test]
    fn repeated_blank_package_vars_do_not_share_one_identity() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        fix.declaration(KIND_VAR, b"_", Some(int));
        fix.declaration(KIND_VAR, b"Keep", Some(int));
        fix.declaration(KIND_VAR, b"_", Some(int));
        let ir = lower_ir(&fix, b"package blank\n")?;
        distinct_identities(&ir, EntityKind::Static, b"_")
    }

    /// A source may spell several carriers with Go's blank identifier, exactly
    /// as go-cmp's `filter(_ *state, _ reflect.Type, _, _ reflect.Value)`
    /// does. Each blank takes its positional spelling, so four same-typed
    /// blanks frame four distinct identities.
    #[test]
    fn explicit_blank_parameter_names_take_positional_spellings() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let brew = fix.func(&[int, int, int], &[int], false);
        for ordinal in 0..4 {
            fix.name_signature_parameter(brew, ordinal, b"_");
        }
        fix.declaration(KIND_FUNC, b"Blank", Some(brew));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        for name in [b"_".as_slice(), b"_1", b"_2", b"_3"] {
            entity_of(&view, name)?;
        }
        Ok(())
    }

    #[test]
    fn capacity_beyond_the_lane_rejects_exactly() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        for index in 0..crate::driver::lower::MAX_EMISSION_FACTS {
            let mut spelling = b"t".to_vec();
            spelling.extend_from_slice(index.to_string().as_bytes());
            let name = fix.atom(&spelling);
            fix.declarations.push(DeclRow {
                kind: KIND_TYPE,
                name,
                package: Cell {
                    offset: fix.atom_cell(PACKAGE).offset,
                    length: fix.atom_cell(PACKAGE).length,
                },
                type_root: None,
                value: Cell {
                    offset: 0,
                    length: 0,
                },
                const_group: 0,
                iota: false,
                span: (0, SPAN_END),
                name_span: None,
                bound: false,
            });
        }
        let exact_image = fix.encode(b"package demo\n")?;
        let mut exact_facts = FactSet::new();
        collect(b"package demo\n", &exact_image, &mut exact_facts).map_err(TestError::Collect)?;

        let index = crate::driver::lower::MAX_EMISSION_FACTS;
        let mut spelling = b"t".to_vec();
        spelling.extend_from_slice(index.to_string().as_bytes());
        let name = fix.atom(&spelling);
        fix.declarations.push(DeclRow {
            kind: KIND_TYPE,
            name,
            package: Cell {
                offset: fix.atom_cell(PACKAGE).offset,
                length: fix.atom_cell(PACKAGE).length,
            },
            type_root: None,
            value: Cell {
                offset: 0,
                length: 0,
            },
            const_group: 0,
            iota: false,
            span: (0, SPAN_END),
            name_span: None,
            bound: false,
        });
        let image = fix.encode(b"package demo\n")?;
        let mut facts = FactSet::new();
        match collect(b"package demo\n", &image, &mut facts) {
            Err(GoCollectError::Rejected(rejection))
                if rejection.fact == crate::driver::lower::MAX_EMISSION_FACTS
                    && rejection.name_len == 1 + index.to_string().len()
                    && rejection.cause == FactFault::Capacity => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("capacity rejection")),
        }
        Ok(())
    }

    #[test]
    fn member_docs_reach_their_pushed_field_facts() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let named = fix.named(PACKAGE, b"int", &[]);
        let int = fix.basic(b"int");
        let _ = named;
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"size", Some(int));
        fix.declarations[node].type_root = Some(struct_row);
        fix.doc(DOC_MEMBER, 0, b"The size in bytes.");
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let fact = docs.next().ok_or(TestError::Missing("doc fact"))??;
        if fact.owner.raw != 1 || fact.fragment != DocFragmentInput::Text(b"The size in bytes.") {
            return Err(TestError::Missing("member doc owner"));
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact member docs"));
        }
        Ok(())
    }

    #[test]
    fn satisfactions_project_local_and_foreign_type_reference_occurrences() -> Result<(), TestError>
    {
        let mut fix = Fixture::new();
        let client = fix.declaration(KIND_TYPE, b"Client", None);
        let reader = fix.declaration(KIND_TYPE, b"Reader", None);
        let _ = reader;
        fix.satisfaction(client, b"Reader", b"");
        fix.satisfaction(client, b"Service", b"example.com/demo/sub");
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Client, 1 Reader. Both satisfaction edges belong to fact 0.
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let local = occurrences
            .next()
            .ok_or(TestError::Missing("local satisfaction"))??;
        if local.owner.raw != 0
            || local.occurrence.target != OccurrenceTarget::Local(EntityId::new(1))
            || local.occurrence.kind != ReferenceKind::TypeReference
            || local.occurrence.confidence != OccurrenceConfidence::Oracle
            || local.occurrence.span.start != 0
            || local.occurrence.span.end != 0
        {
            return Err(TestError::Missing("local satisfaction occurrence"));
        }
        let foreign = occurrences
            .next()
            .ok_or(TestError::Missing("foreign satisfaction"))??;
        let OccurrenceTarget::Foreign(key) = foreign.occurrence.target else {
            return Err(TestError::Missing("foreign satisfaction target"));
        };
        let ForeignOrigin::Package(lineage) = key.origin else {
            return Err(TestError::Missing("foreign satisfaction lineage"));
        };
        if lineage.ecosystem != ECOSYSTEM
            || lineage.name != "example.com/demo/sub"
            || key.path != "Service"
            || foreign.occurrence.kind != ReferenceKind::TypeReference
        {
            return Err(TestError::Missing("foreign satisfaction key"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("exact satisfaction occurrences"));
        }
        // Falsifier: an in-package satisfaction target naming no pushed
        // declaration is a typed rejection, never a silently dropped edge.
        let mut mutated = fix.clone();
        mutated.satisfactions[0].target = mutated.atom(b"Ghost");
        let image = mutated.encode(b"package demo\n")?;
        let mut facts = FactSet::new();
        if collect(b"package demo\n", &image, &mut facts).is_ok() {
            return Err(TestError::Missing("unresolved satisfaction rejection"));
        }
        Ok(())
    }

    #[test]
    fn satisfaction_target_resolves_in_declaring_package() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let keeper = fix.declaration(KIND_TYPE, b"Keeper", None);
        let keeper_type = fix.start_row(ROW_INTERFACE);
        fix.declarations[keeper].type_root = Some(keeper_type);
        let subject = fix.declaration(KIND_TYPE, b"Extra", None);
        fix.declarations[subject].package = fix.atom(b"example.com/demo/extra");
        fix.satisfaction(subject, b"Keeper", PACKAGE);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("satisfaction occurrence"))??;
        if occurrence.owner.raw != 1
            || occurrence.occurrence.target != OccurrenceTarget::Local(EntityId::new(0))
            || occurrence.occurrence.kind != ReferenceKind::TypeReference
            || occurrence.occurrence.confidence != OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing("declaring-package satisfaction target"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("exact satisfaction occurrences"));
        }
        Ok(())
    }

    /// The widened reference plane: every closed use kind maps to its
    /// canonical reference kind, receiver-qualified members resolve to their
    /// exact local facts, and unresolved and foreign targets stay typed
    /// foreign `go` lineage keys under the used object's class.
    #[test]
    fn widened_occurrences_carry_kinds_member_targets_and_typed_foreign_keys()
    -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let lang = fix.declaration(KIND_TYPE, b"Lang", None);
        let greeter = fix.declaration(KIND_TYPE, b"Greeter", None);
        let state = fix.declaration(KIND_VAR, b"state", None);
        let use_fn = fix.declaration(KIND_FUNC, b"Use", None);
        let set_name = fix.method(lang, b"SetName", None);
        let string_row = fix.basic(b"string");
        let lang_row = fix.start_row(ROW_STRUCT);
        fix.field(lang_row, b"Name", Some(string_row));
        fix.declarations[lang].type_root = Some(lang_row);
        let iface_row = fix.start_row(ROW_INTERFACE);
        fix.interface_method(iface_row, b"Read", None);
        fix.declarations[greeter].type_root = Some(iface_row);
        let _ = (set_name, state);
        // Facts: 0 Lang, 1 Greeter, 2 Name (field), 3 SetName (method),
        // 4 Read (interface method), 5 state, 6 Use.

        // One method-owned field write inside SetName: the receiver atom
        // routes the row to the method row spanning the site.
        fix.reference_typed(lang as u32, b"l", b"Name", b"", 32, 36, 1, 2, b"Lang");
        // A method call through a receiver, resolved member-exactly.
        fix.reference_typed(use_fn as u32, b"", b"SetName", b"", 48, 55, 0, 1, b"Lang");
        // A field read through a receiver.
        fix.reference_typed(use_fn as u32, b"", b"Name", b"", 66, 70, 1, 2, b"Lang");
        // An unresolved same-package member: typed foreign key under the
        // declaring package's lineage, never a dropped row.
        fix.reference_typed(use_fn as u32, b"", b"Ghost", b"", 80, 85, 1, 2, b"Lang");
        // A package-level variable read.
        fix.reference_typed(use_fn as u32, b"", b"state", b"", 96, 101, 1, 3, b"");
        // A type use.
        fix.reference_typed(use_fn as u32, b"", b"Greeter", b"", 112, 119, 2, 5, b"");
        // An imported package binding.
        fix.reference_typed(use_fn as u32, b"", b"fmt", b"fmt", 130, 133, 3, 6, b"");
        // A foreign free-function call: the exact v3 row shape.
        fix.reference(use_fn as u32, b"", b"Println", b"fmt", 134, 141);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;

        let write = occurrences
            .next()
            .ok_or(TestError::Missing("field write"))??;
        if write.owner.raw != 3
            || write.occurrence.target != OccurrenceTarget::Local(EntityId::new(2))
            || write.occurrence.kind != ReferenceKind::FieldAccess
            || write.occurrence.span != (RelSpan::new(32, 36).map_err(|_| TestError::Tail)?)
        {
            return Err(TestError::Missing("method-owned field write"));
        }
        let method_call = occurrences
            .next()
            .ok_or(TestError::Missing("method call"))??;
        if method_call.owner.raw != 6
            || method_call.occurrence.target != OccurrenceTarget::Local(EntityId::new(3))
            || method_call.occurrence.kind != ReferenceKind::MethodCall
        {
            return Err(TestError::Missing("receiver method call"));
        }
        let field_read = occurrences
            .next()
            .ok_or(TestError::Missing("field read"))??;
        if field_read.occurrence.target != OccurrenceTarget::Local(EntityId::new(2))
            || field_read.occurrence.kind != ReferenceKind::FieldAccess
        {
            return Err(TestError::Missing("receiver field read"));
        }
        let ghost = occurrences.next().ok_or(TestError::Missing("ghost"))??;
        let OccurrenceTarget::Foreign(ref key) = ghost.occurrence.target else {
            return Err(TestError::Missing("unresolved member stays foreign"));
        };
        let ForeignOrigin::Package(ref lineage) = key.origin else {
            return Err(TestError::Missing("foreign lineage"));
        };
        if lineage.ecosystem != ECOSYSTEM
            || lineage.name != "example.com/demo"
            || key.path != "Ghost"
            || key.kind != Some(EntityKind::Field)
            || ghost.occurrence.kind != ReferenceKind::FieldAccess
        {
            return Err(TestError::Missing("typed foreign member key"));
        }
        let var_read = occurrences.next().ok_or(TestError::Missing("var read"))??;
        if var_read.occurrence.target != OccurrenceTarget::Local(EntityId::new(5))
            || var_read.occurrence.kind != ReferenceKind::VariableUse
        {
            return Err(TestError::Missing("variable use"));
        }
        let type_use = occurrences.next().ok_or(TestError::Missing("type use"))??;
        if type_use.occurrence.target != OccurrenceTarget::Local(EntityId::new(1))
            || type_use.occurrence.kind != ReferenceKind::TypeReference
        {
            return Err(TestError::Missing("type use"));
        }
        let import = occurrences.next().ok_or(TestError::Missing("import"))??;
        let OccurrenceTarget::Foreign(ref key) = import.occurrence.target else {
            return Err(TestError::Missing("import target"));
        };
        let ForeignOrigin::Package(ref lineage) = key.origin else {
            return Err(TestError::Missing("import lineage"));
        };
        if lineage.ecosystem != ECOSYSTEM
            || lineage.name != "fmt"
            || key.path != "fmt"
            || key.kind != Some(EntityKind::Module)
            || import.occurrence.kind != ReferenceKind::Import
        {
            return Err(TestError::Missing("import key"));
        }
        let foreign_call = occurrences
            .next()
            .ok_or(TestError::Missing("foreign call"))??;
        let OccurrenceTarget::Foreign(ref key) = foreign_call.occurrence.target else {
            return Err(TestError::Missing("foreign call target"));
        };
        if key.path != "Println"
            || key.kind != Some(EntityKind::Function)
            || foreign_call.occurrence.kind != ReferenceKind::FunctionCall
        {
            return Err(TestError::Missing("foreign call key"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("exact occurrences"));
        }
        Ok(())
    }

    /// The owned IR carries every widened occurrence with an absolute source
    /// site that verifies against the exact identifier bytes, and the
    /// satisfaction edge anchors on the subject's declared NAME-TOKEN
    /// extent instead of the position-free zero-width spelling.
    #[test]
    fn ir_links_carry_source_verified_occurrence_sites() -> Result<(), TestError> {
        use backend_semantic::ir::{LinkKind, LinkTarget, SemanticReader as _};
        let source: &[u8] = b"package demo\n\ntype Lang struct {\n\tName string\n}\n\nfunc (l *Lang) SetName() {\n}\n\ntype Greeter interface {\n\tRead()\n}\n\nfunc Use() {\n\tl := Lang{}\n\tl.SetName()\n\tvar g Greeter = l\n\t_ = g\n}\n";
        let mut fix = Fixture::new();
        let lang = fix.declaration(KIND_TYPE, b"Lang", None);
        let greeter = fix.declaration(KIND_TYPE, b"Greeter", None);
        let use_fn = fix.declaration(KIND_FUNC, b"Use", None);
        let set_name = fix.method(lang, b"SetName", None);
        let string_row = fix.basic(b"string");
        let lang_row = fix.start_row(ROW_STRUCT);
        fix.field(lang_row, b"Name", Some(string_row));
        fix.declarations[lang].type_root = Some(lang_row);
        let iface_row = fix.start_row(ROW_INTERFACE);
        fix.interface_method(iface_row, b"Read", None);
        fix.declarations[greeter].type_root = Some(iface_row);
        // Authority-bound rows with their exact declaration extents.
        let (lang_start, _) = at(source, b"type Lang")?;
        let (lang_name_start, lang_name_end) = at(source, b"Lang")?;
        fix.declarations[lang].span = (lang_start, at(source, b"\n\nfunc (l")?.0);
        fix.declarations[lang].name_span = Some((lang_name_start, lang_name_end));
        fix.declarations[lang].bound = true;
        let (greeter_start, _) = at(source, b"type Greeter")?;
        fix.declarations[greeter].span = (greeter_start, at(source, b"\n\nfunc Use")?.0);
        fix.declarations[greeter].name_span = Some(at(source, b"Greeter")?);
        fix.declarations[greeter].bound = true;
        fix.declarations[use_fn].span = (at(source, b"func Use")?.0, source.len() as u32);
        fix.declarations[use_fn].name_span = Some(at(source, b"Use")?);
        fix.declarations[use_fn].bound = true;
        fix.methods[set_name].span = (
            at(source, b"func (l *Lang) SetName")?.0,
            at(source, b"func (l *Lang) SetName() {\n}\n\n")?.1,
        );
        fix.methods[set_name].bound = true;
        // Facts: 0 Lang, 1 Greeter, 2 Name, 3 SetName, 4 Read, 5 Use.
        // Reference rows, ascending by site: the type use inside the
        // composite literal, the method call, and the satisfaction edge.
        let (site, end) = at(source, b"Lang{}")?;
        let (site, end) = (site, end - 2);
        fix.reference_typed(use_fn as u32, b"", b"Lang", b"", site, end, 2, 5, b"");
        let (site, end) = at(source, b"l.SetName()")?;
        let (site, end) = (site + 2, end - 2);
        fix.reference_typed(
            use_fn as u32,
            b"",
            b"SetName",
            b"",
            site,
            end,
            0,
            1,
            b"Lang",
        );
        let (greeter_site, greeter_end) = at(source, b"Greeter = l")?;
        let (greeter_site, greeter_end) = (greeter_site, greeter_end - 4);
        fix.reference_typed(
            use_fn as u32,
            b"",
            b"Greeter",
            b"",
            greeter_site,
            greeter_end,
            2,
            5,
            b"",
        );
        fix.satisfaction(lang, b"Greeter", b"");
        let ir = lower_ir(&fix, source)?;

        // Entities carry their authority-bound declaration spans.
        let mut lang_source = None;
        for row in ir.canonical_entities() {
            let name = ir.atom(row.name).ok_or(TestError::Missing("entity atom"))?;
            if name == b"Lang" {
                lang_source = row.source;
            }
        }
        let lang_span = lang_source.ok_or(TestError::Missing("Lang entity source span"))?;
        if &source[lang_span.start() as usize..lang_span.start() as usize + 9] != b"type Lang" {
            return Err(TestError::Missing("Lang entity span basis"));
        }

        // Every link occurrence carries an absolute site whose bytes are
        // exactly the used identifier, and each kind matches its row's
        // closed class.
        let mut sites = Vec::new();
        for (_, occurrence) in ir.link_occurrences() {
            let Some(site) = occurrence.source else {
                return Err(TestError::Missing("absolute occurrence site"));
            };
            let link = ir
                .link(occurrence.link)
                .ok_or(TestError::Missing("occurrence link"))?;
            let bytes = &source[site.start() as usize..site.end() as usize];
            sites.push((bytes.to_vec(), link.kind, link.target));
        }
        if sites.len() != 4 {
            return Err(TestError::Missing("exact link occurrences"));
        }
        let (bytes, kind, target) = &sites[0];
        if bytes != b"Lang"
            || *kind != LinkKind::TypeReference
            || *target != LinkTarget::Local(EntityId::new(0))
        {
            return Err(TestError::Missing("type-use site"));
        }
        let (bytes, kind, target) = &sites[1];
        if bytes != b"SetName"
            || *kind != LinkKind::MethodCall
            || *target != LinkTarget::Local(EntityId::new(3))
        {
            return Err(TestError::Missing("method-call site"));
        }
        let (bytes, kind, target) = &sites[2];
        if bytes != b"Greeter"
            || *kind != LinkKind::TypeReference
            || *target != LinkTarget::Local(EntityId::new(1))
        {
            return Err(TestError::Missing("satisfaction site"));
        }
        let (bytes, kind, target) = &sites[3];
        if bytes != b"Lang"
            || *kind != LinkKind::TypeReference
            || *target != LinkTarget::Local(EntityId::new(1))
        {
            return Err(TestError::Missing("satisfaction NAME-TOKEN site"));
        }
        Ok(())
    }

    #[test]
    fn projected_constant_facts_carry_exact_value_group_and_iota() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        fix.constant(b"First", Some(int), b"0", 1);
        let with_values = lower(&fix, b"package demo\nconst First = 0\n")?;
        let view = FragmentView::validate(&with_values)?;
        let facts = go_extension(&view, 0)?;
        if facts.constant_group != 1 || facts.constant_flags != 1 {
            return Err(TestError::Missing("constant group and iota"));
        }
        if facts.constant_value.raw != 1 {
            return Err(TestError::Missing("constant value atom list"));
        }
        let listed = pooled_list(
            &view,
            backend_semantic::ir::ExtensionPoolListLane::Atoms,
            facts.constant_value.raw,
        )?;
        if listed.len() != 1 {
            return Err(TestError::Missing("one constant value atom"));
        }
        let atom = view
            .atoms()
            .nth(usize::try_from(listed[0]).map_err(|_| TestError::Tail)?)
            .ok_or(TestError::Missing("constant value atom"))?;
        if atom.bytes != b"0" {
            return Err(TestError::Missing("exact constant value"));
        }
        // Falsifier: changing the image value changes the projected fragment.
        let mut mutated = fix.clone();
        mutated.declarations[0].value = mutated.atom(b"255");
        let other = lower(&mutated, b"package demo\nconst First = 0\n")?;
        if other == with_values {
            return Err(TestError::Missing("value-sensitive projection"));
        }
        Ok(())
    }

    #[test]
    fn constants_outside_groups_and_nonconstants_keep_zero_constant_cells() -> Result<(), TestError>
    {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let ungrouped_index = fix.constant(b"Ungrouped", Some(int), b"7", 0);
        fix.declarations[ungrouped_index].iota = false;
        fix.declaration(KIND_TYPE, b"Named", None);
        fix.declaration(KIND_FUNC, b"run", None);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let named = go_extension(&view, 0)?;
        if named.constant_value.raw != 0 || named.constant_group != 0 || named.constant_flags != 0 {
            return Err(TestError::Missing("type constant cells"));
        }
        let function = go_extension(&view, 2)?;
        if function.constant_value.raw != 0
            || function.constant_group != 0
            || function.constant_flags != 0
        {
            return Err(TestError::Missing("function constant cells"));
        }
        let ungrouped = go_extension(&view, 1)?;
        if ungrouped.constant_value.raw != 1 {
            return Err(TestError::Missing("ungrouped value coordinate"));
        }
        if ungrouped.constant_group != 0 {
            return Err(TestError::Missing("ungrouped group"));
        }
        if ungrouped.constant_flags != 0 {
            return Err(TestError::Missing("ungrouped flags"));
        }
        if pooled_list(
            &view,
            backend_semantic::ir::ExtensionPoolListLane::Atoms,
            ungrouped.constant_value.raw,
        )?
        .len()
            != 1
        {
            return Err(TestError::Missing("ungrouped value atom"));
        }
        Ok(())
    }

    #[test]
    fn unresolved_cgo_plane_projects_foreign_names() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Conn", None);
        fix.unresolved_cgo(b"C.sqlite3");
        fix.unresolved_cgo(b"example.com/cgo.Conn");
        let source = b"package cgo\n";
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let conn_count = view
            .entities()
            .filter(|entity| {
                usize::try_from(entity.name.raw)
                    .ok()
                    .and_then(|index| view.atoms().nth(index))
                    .is_some_and(|atom| atom.bytes == b"Conn")
            })
            .count();
        if conn_count != 1 {
            return Err(TestError::Missing("exactly one Conn entity"));
        }
        let sqlite = row_for_name(&view, b"C.sqlite3")?;
        if entity_kind_of(&view, b"C.sqlite3")? != EntityKind::Alias
            || sqlite.record.tag != SemanticTypeTag::Unknown
            || sqlite.record.payload0 != TypeReason::UnresolvedExternal as u32
            || sqlite.record.text != Some(b"C.sqlite3".as_slice())
        {
            return Err(TestError::Missing("foreign cgo alias type"));
        }
        if entity_of(&view, b"example.com/cgo.Conn").is_ok() {
            return Err(TestError::Missing(
                "duplicate Conn must not project from unresolved cgo plane",
            ));
        }
        Ok(())
    }

    #[test]
    fn package_docs_stay_image_only_without_owner_facts() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Node", None);
        fix.doc(DOC_PACKAGE, 0, b"Package demo exercises every plane.");
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        if view
            .docs()
            .is_some_and(|mut cursor| cursor.next().is_some())
        {
            return Err(TestError::Missing("package doc projection"));
        }
        Ok(())
    }

    #[test]
    fn pooled_entity_lists_admit_the_exact_measured_bound_then_reject() -> Result<(), TestError> {
        let mut facts = FactSet::new();
        for _ in 0..MAX_REF_LISTS - 1 {
            if facts
                .push(SemanticFact::new(
                    EntityKind::Record,
                    b"row",
                    SemanticProductConstructor::PRODUCT,
                ))
                .is_err()
            {
                return Err(TestError::Missing("fact setup capacity"));
            }
        }

        for index in 0..MAX_REF_LISTS - 1 {
            if facts.intern_entity_list(&[index as u32]).is_err() {
                return Err(TestError::Missing("pooled entity list setup"));
            }
        }
        if facts.intern_entity_list(&[0, 1]).is_err() {
            return Err(TestError::Missing("exact pooled entity-list bound"));
        }
        if !matches!(
            facts.intern_entity_list(&[0, 1, 2]),
            Err(FactFault::RefListCapacity)
        ) {
            return Err(TestError::Missing("pooled entity-list capacity rejection"));
        }
        Ok(())
    }
}
