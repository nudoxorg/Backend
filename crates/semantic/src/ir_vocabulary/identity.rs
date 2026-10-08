//! Cross-fragment reference and declaration-identity vocabulary.
//!
//! A declaration's stable identity is minted from producer-visible facts
//! alone — `(package lineage, path, kind, name)` — hashed
//! through dedicated declaration identity domains. There is deliberately **no ordinal
//! disambiguator and no span disambiguator**: the old system's measured
//! 32,339 ordinal-keyed identity groups (446,947 declarations, 79 package
//! versions) and its degenerate `0..0` span era are the defects this module
//! exists to never reproduce.
//!
//! [`StableRef`] is the only cross-fragment reference form; [`ForeignKey`]
//! is the self-describing placeholder a producer records when the target is
//! not loaded. Foreign keys hash their **key cells, never a resolved
//! target**: sealing with and without dependencies loaded is byte-identical,
//! so `display` is excluded from the key digest.

use core::{mem::size_of, ops::Deref};

use backend_version::{
    ContentId, DeclarationFamilyDomain, DeclarationKeyDomain, DeclarationVariantDomain,
    ForeignDeclarationDomain,
};

use crate::ir_vocabulary::coordinates::EntityId;
use crate::ir_vocabulary::entity::EntityKind;

/// Purpose tag naming the declaration-key preimage inside its dedicated
/// declaration-key identity domain.
const DECLARATION_KEY_PURPOSE: &[u8] = b"compiler.declaration.v2";
/// Purpose tag naming the foreign-key digest preimage inside the shared
/// foreign-declaration identity domain.
const FOREIGN_KEY_PURPOSE: &[u8] = b"compiler.foreign-key.v1";
/// Maximum enclosing syntax roles retained by one callable route.
pub const MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS: usize = 32;
/// Maximum source-backed bytes copied into one anonymous callable route.
pub const MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES: usize = 4096;

/// A package lineage: ecosystem plus package name, stable across all
/// generations (for example `cargo:serde`). This — not any content digest —
/// keys declaration identities, so identities never depend on the fragment
/// bytes they are later embedded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLineageView<'bytes> {
    /// Ecosystem registry name (`cargo`, `npm`, `pypi`, `go`, `nuget`,
    /// `maven`, ...).
    pub ecosystem: &'bytes str,
    /// Package name inside that ecosystem.
    pub name: &'bytes str,
}

/// Validated package lineage used by every identity-bearing key.  The raw
/// fields live in [`PackageLineageView`], which can only enter this owner
/// through [`PackageLineage::new`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLineage<'bytes>(PackageLineageView<'bytes>);

/// Exact lineage rejection retaining the offending part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageLineageFault {
    /// The ecosystem segment is empty.
    EmptyEcosystem,
    /// The package name is empty.
    EmptyName,
    /// The ecosystem segment contains the lineage render separator `:`.
    SeparatorInEcosystem,
    /// The package name contains the lineage render separator `:`.
    SeparatorInName,
    /// A segment contains a path separator, which would make keyed paths
    /// ambiguous across ecosystems.
    Backslash {
        /// Offending segment (`0` = ecosystem, `1` = package name).
        segment: u8,
    },
}

impl<'bytes> PackageLineage<'bytes> {
    /// Validates one lineage: both segments non-empty, neither containing
    /// the `ecosystem:name` render separator or a path separator.
    pub const fn new(
        ecosystem: &'bytes str,
        name: &'bytes str,
    ) -> Result<Self, PackageLineageFault> {
        if ecosystem.is_empty() {
            return Err(PackageLineageFault::EmptyEcosystem);
        }
        if name.is_empty() {
            return Err(PackageLineageFault::EmptyName);
        }
        if contains_colon(ecosystem) {
            return Err(PackageLineageFault::SeparatorInEcosystem);
        }
        if contains_colon(name) {
            return Err(PackageLineageFault::SeparatorInName);
        }
        if contains_backslash(ecosystem.as_bytes()) {
            return Err(PackageLineageFault::Backslash { segment: 0 });
        }
        if contains_backslash(name.as_bytes()) {
            return Err(PackageLineageFault::Backslash { segment: 1 });
        }
        Ok(Self(PackageLineageView { ecosystem, name }))
    }
}

impl<'bytes> Deref for PackageLineage<'bytes> {
    type Target = PackageLineageView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

const fn contains_colon(text: &str) -> bool {
    contains_byte(text.as_bytes(), b':')
}

const fn contains_backslash(bytes: &[u8]) -> bool {
    contains_byte(bytes, b'\\')
}

const fn contains_byte(bytes: &[u8], needle: u8) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == needle {
            return true;
        }
        index += 1;
    }
    false
}

/// Exact declaration-path rejection retaining the observed operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationPathFault {
    /// The package-relative path is empty.
    Empty,
    /// The path contains a backslash; callers normalize to `/` first.
    Backslash,
}

/// Every producer-visible fact that mints one declaration's stable identity.
///
/// The identity is `(lineage, path, kind, name)`-keyed — never ordinal-keyed
/// and never content-of-fragment-keyed — so two compilations that declare
/// the same fact in a different order, or with different neighbors, mint the
/// identical id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationKey<'bytes> {
    /// Owning package lineage.
    pub lineage: PackageLineage<'bytes>,
    /// Package-relative, `/`-separated source path of the declaring file.
    pub path: &'bytes str,
    /// Declaration kind discriminant.
    pub kind: EntityKind,
    /// Exact declaration name bytes.
    pub name: &'bytes [u8],
}

/// Exact preimage-write rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreimageOverflow {
    /// The caller output cannot hold the complete preimage; `needed` is the
    /// complete canonical length and `actual` the supplied width.
    OutputShort {
        /// Complete canonical preimage length the caller must provide room for.
        needed: usize,
        /// Output width the caller actually supplied.
        actual: usize,
    },
    /// A nested canonical byte cell exceeds its fixed `u32` length field.
    CellTooLong {
        /// Observed nested-cell byte length.
        actual: usize,
    },
    /// Adding one validated cell would overflow the platform's addressable
    /// preimage width.  The exact partial total and requested cell width are
    /// retained before any caller allocates scratch.
    AggregateTooLong {
        /// Canonical bytes already accounted for.
        accumulated: usize,
        /// Width of the next complete cell or fixed segment.
        additional: usize,
    },
}

impl<'bytes> DeclarationKey<'bytes> {
    /// Validates one declaration key, proving the path is a non-empty,
    /// `/`-separated package-relative path and the name is non-empty.
    pub const fn new(
        lineage: PackageLineage<'bytes>,
        path: &'bytes str,
        kind: EntityKind,
        name: &'bytes [u8],
    ) -> Result<Self, DeclarationKeyFault> {
        // `PackageLineage` is an opaque validated owner.  Keep this explicit
        // constructor boundary so every declaration key remains the one
        // place that proves its package fact before it can mint identity.
        if path.is_empty() {
            return Err(DeclarationKeyFault::Path(DeclarationPathFault::Empty));
        }
        if contains_backslash(path.as_bytes()) {
            return Err(DeclarationKeyFault::Path(DeclarationPathFault::Backslash));
        }
        if name.is_empty() {
            return Err(DeclarationKeyFault::EmptyName);
        }
        Ok(Self {
            lineage,
            path,
            kind,
            name,
        })
    }

    /// Complete family-only canonical preimage length.
    pub fn preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let mut length = cell_len(DECLARATION_KEY_PURPOSE)?;
        length = add_preimage_len(length, cell_len(self.lineage.ecosystem.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.lineage.name.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.path.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.name)?)?;
        add_preimage_len(length, KEY_TAIL_BYTES)
    }

    /// Writes the complete canonical preimage into `out` and returns the
    /// exact byte count written. Every variable-length field is
    /// length-prefixed, so distinct keys never share a preimage by
    /// concatenation. Output is written only after the total-length
    /// preflight; a short output leaves every byte untouched.
    pub fn write_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let mut cursor = 0;
        cursor = write_str_cell(out, cursor, DECLARATION_KEY_PURPOSE)?;
        cursor = write_str_cell(out, cursor, self.lineage.ecosystem.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.lineage.name.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.path.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.name)?;
        out[cursor..cursor + KEY_TAIL_BYTES].copy_from_slice(&u16::from(self.kind).to_le_bytes());
        cursor += KEY_TAIL_BYTES;
        Ok(cursor)
    }

    /// Mints the declaration's stable identity through the central
    /// `heart/identity` domain conventions. `out` is caller-owned scratch
    /// holding the canonical preimage; see [`DeclarationKey::write_preimage`]
    /// for the exact width requirement.
    pub fn stable_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<DeclarationKeyDomain>, PreimageOverflow> {
        let written = self.write_preimage(out)?;
        Ok(ContentId::<DeclarationKeyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

/// Exact declaration-key rejection retaining the offending part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKeyFault {
    /// The package-relative path is malformed.
    Path(DeclarationPathFault),
    /// The declaration name is empty.
    EmptyName,
}

/// Typed declaration name used by producers that can prove an anonymous
/// callable but do not have a source identifier for it. The `Named` arm is
/// exactly the existing declaration-key input; the anonymous arm is a
/// structural source route and never manufactures identifier bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationName<'bytes> {
    /// Exact source spelling used by the existing declaration-key encoding.
    Named(&'bytes [u8]),
    /// A source-structural anchor for an anonymous function, arrow, or
    /// function type. This is not identifier text.
    AnonymousCallable(AnonymousCallableAnchor<'bytes>),
}

/// Stable source syntax route for one anonymous callable.
///
/// Source spans, node IDs, child ordinals, and callable body bytes are
/// intentionally absent. The surrounding [`TypedDeclarationKey`] and
/// [`ScopedTypedDeclarationKey`](crate::ir::ScopedTypedDeclarationKey) bind
/// the route to its package path, Function kind, language profile, and exact
/// lexical parent identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnonymousCallableAnchor<'bytes> {
    /// Ordered source-structural steps from the callable's enclosing stable
    /// scope down to this callable. Identical routes remain identical and
    /// must be represented as one ambiguous callable family by the consumer.
    pub steps: &'bytes [CallableAnchorStep<'bytes>],
}

/// Count of current exact instances that share one durable anonymous family.
/// Duplicate structural anchors remain useful facts and are represented as
/// an explicit ambiguity instead of being merged or assigned an ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnonymousCallableFamilyMultiplicity {
    /// Exactly one current instance has this stable structural family.
    Unique,
    /// Several exact instances share this stable structural family.
    Ambiguous {
        /// Number of current instances, including the selected row.
        instance_count: u32,
    },
}

impl AnonymousCallableFamilyMultiplicity {
    /// Validates the member count retained by one anonymous family anchor.
    pub const fn new(instance_count: u32) -> Option<Self> {
        if instance_count == 1 {
            Some(Self::Unique)
        } else if instance_count > 1 {
            Some(Self::Ambiguous { instance_count })
        } else {
            None
        }
    }
}

/// One parent syntax shape and the callable's typed role within that shape.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CallableAnchorStep<'bytes> {
    /// Syntactic role occupied by the callable child.
    pub child_role: CallableChildRole,
    /// Source-backed, coordinate-free shape of the child parent.
    pub parent: CallableParentShape<'bytes>,
}

/// Closed syntactic roles supported in a structural callable route.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CallableChildRole {
    /// Callable is an argument to a source call expression.
    CallArgument,
    /// Callable is the initializer of a named binding.
    VariableInitializer,
    /// Callable is the value of a named property.
    PropertyValue,
    /// Callable is the true branch of a conditional expression.
    ConditionalConsequent,
    /// Callable is the false branch of a conditional expression.
    ConditionalAlternate,
    /// Callable is an element of an array literal.
    ArrayElement,
    /// Callable is the value of a named object-literal property.
    ObjectMemberValue,
    /// Callable type is the type annotation of a named signature parameter.
    SignatureParameterType,
    /// Callable type is a child of a typed type-expression node.
    TypeExpression,
    /// Callable type is the right-hand side of a named type alias.
    TypeAliasValue,
    /// Anonymous call signature is a member of a type container.
    CallSignatureMember,
    /// Anonymous construct signature is a member of a type container.
    ConstructSignatureMember,
}

/// Source-backed shape of the syntax node that directly contains a callable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CallableParentShape<'bytes> {
    /// A call expression. Generic callee spellings and arguments are source
    /// values, not declaration identity operands.
    Call,
    /// A variable declaration with its exact source-written binding name.
    VariableBinding(&'bytes [u8]),
    /// A property assignment with its exact source-written property name.
    PropertyName(&'bytes [u8]),
    /// A conditional expression with no additional stable source token.
    Conditional,
    /// An array literal with no additional stable source token.
    ArrayLiteral,
    /// An object-literal property with its exact source-written key.
    ObjectMember(&'bytes [u8]),
    /// A signature parameter with its exact source-written name.
    SignatureParameter(&'bytes [u8]),
    /// A typed type-syntax container that directly owns the callable child.
    TypeContainer(CallableTypeContainerKind),
    /// A type alias with its exact source-written name.
    TypeAliasName(&'bytes [u8]),
}

/// Closed syntax kinds allowed as structural parents of anonymous type
/// callables. This records node kind and exact AST membership, never a child
/// ordinal.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CallableTypeContainerKind {
    /// `T[]` or another array type expression.
    Array = 0,
    /// Tuple type expression.
    Tuple = 1,
    /// Union type expression.
    Union = 2,
    /// Intersection type expression.
    Intersection = 3,
    /// Parenthesized type expression.
    Parenthesized = 4,
    /// Optional type expression.
    Optional = 5,
    /// Rest type expression.
    Rest = 6,
    /// Function type expression.
    Function = 7,
    /// Constructor type expression.
    Constructor = 8,
    /// Generic type reference containing an argument.
    TypeReference = 9,
    /// Anonymous type literal containing a call or construct signature.
    TypeLiteral = 10,
    /// Interface declaration containing a call or construct signature.
    Interface = 11,
}

/// Exact validation failure for a typed declaration name or callable anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypedDeclarationKeyFault {
    /// The named declaration arm failed existing key validation.
    Named(DeclarationKeyFault),
    /// The anonymous callable arm is valid only for Function entities.
    AnonymousCallableKind(EntityKind),
    /// The anonymous callable route has no structural steps.
    EmptyAnchor,
    /// The source route exceeds its bounded structural depth.
    AnchorTooDeep {
        /// Observed number of structural steps.
        actual: usize,
        /// Maximum accepted number of structural steps.
        limit: usize,
    },
    /// The source route contains more copied text than its bounded pool row.
    AnchorTextTooLarge {
        /// Observed number of source-backed bytes.
        actual: usize,
        /// Maximum accepted number of source-backed bytes.
        limit: usize,
    },
    /// One source-backed token is empty.
    EmptyStructuralToken,
    /// The recorded child role does not match its parent-shape variant.
    RoleShapeMismatch {
        /// Exact role carried by the source witness.
        child_role: CallableChildRole,
        /// Exact parent-shape tag carried by the source witness.
        parent_shape: CallableParentShapeTag,
    },
}

/// Closed parent-shape tag retained in an exact validation fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallableParentShapeTag {
    /// Parent is a call expression.
    Call,
    /// Parent is a variable binding.
    VariableBinding,
    /// Parent is a property assignment.
    PropertyName,
    /// Parent is a conditional expression.
    Conditional,
    /// Parent is an array literal.
    ArrayLiteral,
    /// Parent is an object member.
    ObjectMember,
    /// Parent is a signature parameter.
    SignatureParameter,
    /// Parent is a typed type-syntax container.
    TypeContainer,
    /// Parent is a named type alias.
    TypeAliasName,
}

impl CallableParentShape<'_> {
    const fn tag(self) -> CallableParentShapeTag {
        match self {
            Self::Call { .. } => CallableParentShapeTag::Call,
            Self::VariableBinding(_) => CallableParentShapeTag::VariableBinding,
            Self::PropertyName(_) => CallableParentShapeTag::PropertyName,
            Self::Conditional => CallableParentShapeTag::Conditional,
            Self::ArrayLiteral => CallableParentShapeTag::ArrayLiteral,
            Self::ObjectMember(_) => CallableParentShapeTag::ObjectMember,
            Self::SignatureParameter(_) => CallableParentShapeTag::SignatureParameter,
            Self::TypeContainer(_) => CallableParentShapeTag::TypeContainer,
            Self::TypeAliasName(_) => CallableParentShapeTag::TypeAliasName,
        }
    }
}

/// Typed declaration key that preserves the legacy name-key bytes for named
/// declarations and provides a separate coordinate-free encoding for an
/// anonymous callable route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypedDeclarationKey<'bytes> {
    /// Owning package lineage.
    lineage: PackageLineage<'bytes>,
    /// Package-relative source path.
    path: &'bytes str,
    /// Closed declaration kind.
    kind: EntityKind,
    /// Source name or typed anonymous callable route.
    name: DeclarationName<'bytes>,
}

// The closed route grammar grows additively: existing tags and framed cells
// retain their original identity bytes, while new tags cannot alias old routes.
const ANONYMOUS_CALLABLE_KEY_PURPOSE: &[u8] = b"compiler.declaration.anonymous-callable.v1";
const ANONYMOUS_CALLABLE_NAME_MAGIC: &[u8; 4] = b"NAC\x02";

impl<'bytes> TypedDeclarationKey<'bytes> {
    /// Validates one typed name while leaving the existing named key intact.
    pub fn new(
        lineage: PackageLineage<'bytes>,
        path: &'bytes str,
        kind: EntityKind,
        name: DeclarationName<'bytes>,
    ) -> Result<Self, TypedDeclarationKeyFault> {
        match name {
            DeclarationName::Named(name) => {
                DeclarationKey::new(lineage, path, kind, name)
                    .map_err(TypedDeclarationKeyFault::Named)?;
            }
            DeclarationName::AnonymousCallable(anchor) => {
                validate_path(path).map_err(TypedDeclarationKeyFault::Named)?;
                if kind != EntityKind::Function {
                    return Err(TypedDeclarationKeyFault::AnonymousCallableKind(kind));
                }
                validate_callable_anchor(anchor)?;
            }
        }
        Ok(Self {
            lineage,
            path,
            kind,
            name,
        })
    }

    /// Returns the validated package lineage.
    #[must_use]
    pub const fn lineage(self) -> PackageLineage<'bytes> {
        self.lineage
    }

    /// Returns the validated package-relative source path.
    #[must_use]
    pub const fn path(self) -> &'bytes str {
        self.path
    }

    /// Returns the validated declaration kind.
    #[must_use]
    pub const fn kind(self) -> EntityKind {
        self.kind
    }

    /// Returns the validated source-name case.
    #[must_use]
    pub const fn name(self) -> DeclarationName<'bytes> {
        self.name
    }

    /// Returns the original validated declaration key for its named arm.
    #[must_use]
    pub const fn as_named_key(self) -> Option<DeclarationKey<'bytes>> {
        match self.name {
            DeclarationName::Named(name) => Some(DeclarationKey {
                lineage: self.lineage,
                path: self.path,
                kind: self.kind,
                name,
            }),
            DeclarationName::AnonymousCallable(_) => None,
        }
    }

    /// Complete checked declaration-key preimage width.
    pub fn preimage_len(&self) -> Result<usize, PreimageOverflow> {
        match self.name {
            DeclarationName::Named(_) => self
                .as_named_key()
                .ok_or(PreimageOverflow::AggregateTooLong {
                    accumulated: 0,
                    additional: 0,
                })?
                .preimage_len(),
            DeclarationName::AnonymousCallable(anchor) => {
                let mut length = cell_len(ANONYMOUS_CALLABLE_KEY_PURPOSE)?;
                length = add_preimage_len(length, cell_len(self.lineage.ecosystem.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(self.lineage.name.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(self.path.as_bytes())?)?;
                length = add_preimage_len(length, KEY_TAIL_BYTES)?;
                let _step_count = u32::try_from(anchor.steps.len()).map_err(|_| {
                    PreimageOverflow::CellTooLong {
                        actual: anchor.steps.len(),
                    }
                })?;
                length = add_preimage_len(length, 4)?;
                for step in anchor.steps {
                    length = add_preimage_len(length, 2)?;
                    length = add_preimage_len(length, callable_parent_payload_len(step.parent)?)?;
                }
                Ok(length)
            }
        }
    }

    /// Writes the canonical declaration-key preimage. The named branch
    /// delegates to the byte-stable v2 writer; only anonymous callables use
    /// the new tagged purpose.
    pub fn write_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let DeclarationName::AnonymousCallable(anchor) = self.name else {
            return self
                .as_named_key()
                .ok_or(PreimageOverflow::AggregateTooLong {
                    accumulated: 0,
                    additional: 0,
                })?
                .write_preimage(out);
        };
        let mut cursor = 0;
        cursor = write_str_cell(out, cursor, ANONYMOUS_CALLABLE_KEY_PURPOSE)?;
        cursor = write_str_cell(out, cursor, self.lineage.ecosystem.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.lineage.name.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.path.as_bytes())?;
        out[cursor..cursor + KEY_TAIL_BYTES].copy_from_slice(&u16::from(self.kind).to_le_bytes());
        cursor += KEY_TAIL_BYTES;
        let count =
            u32::try_from(anchor.steps.len()).map_err(|_| PreimageOverflow::CellTooLong {
                actual: anchor.steps.len(),
            })?;
        out[cursor..cursor + 4].copy_from_slice(&count.to_le_bytes());
        cursor += 4;
        for step in anchor.steps {
            out[cursor] = callable_role_tag(step.child_role);
            out[cursor + 1] = callable_parent_tag(step.parent);
            cursor += 2;
            cursor = write_callable_parent(out, cursor, step.parent)?;
        }
        Ok(cursor)
    }

    /// Mints this typed key through the existing declaration-key identity
    /// domain. The purpose tag keeps anonymous encodings disjoint from every
    /// named v2 key.
    pub fn stable_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<DeclarationKeyDomain>, PreimageOverflow> {
        let written = self.write_preimage(out)?;
        Ok(ContentId::<DeclarationKeyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

impl AnonymousCallableAnchor<'_> {
    /// Validates the route independently of package and declaration scope.
    pub fn validate(self) -> Result<(), TypedDeclarationKeyFault> {
        validate_callable_anchor(self)
    }

    /// Bounded encoded width used by typed item-name storage. Callers must
    /// validate this anchor through [`TypedDeclarationKey::new`] first.
    pub fn storage_len(self) -> Result<usize, PreimageOverflow> {
        let mut length = ANONYMOUS_CALLABLE_NAME_MAGIC.len() + 8;
        for step in self.steps {
            length = checked_anchor_storage_length(length, 2)?;
            length =
                checked_anchor_storage_length(length, callable_parent_payload_len(step.parent)?)?;
        }
        Ok(length)
    }

    /// Writes a versioned structural anchor plus its current family
    /// multiplicity. The byte spelling is not a declaration name.
    pub fn write_storage(
        self,
        multiplicity: AnonymousCallableFamilyMultiplicity,
        out: &mut [u8],
    ) -> Result<usize, PreimageOverflow> {
        let needed = self.storage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        out[..4].copy_from_slice(ANONYMOUS_CALLABLE_NAME_MAGIC);
        let count = match multiplicity {
            AnonymousCallableFamilyMultiplicity::Unique => 1,
            AnonymousCallableFamilyMultiplicity::Ambiguous { instance_count } => instance_count,
        };
        out[4..8].copy_from_slice(&count.to_le_bytes());
        let route_count =
            u32::try_from(self.steps.len()).map_err(|_| PreimageOverflow::CellTooLong {
                actual: self.steps.len(),
            })?;
        out[8..12].copy_from_slice(&route_count.to_le_bytes());
        let mut cursor = 12;
        for step in self.steps {
            out[cursor] = callable_role_tag(step.child_role);
            out[cursor + 1] = callable_parent_tag(step.parent);
            cursor += 2;
            cursor = write_callable_parent(out, cursor, step.parent)?;
        }
        Ok(cursor)
    }
}

fn checked_anchor_storage_length(
    accumulated: usize,
    additional: usize,
) -> Result<usize, PreimageOverflow> {
    accumulated
        .checked_add(additional)
        .ok_or(PreimageOverflow::AggregateTooLong {
            accumulated,
            additional,
        })
}

fn validate_path(path: &str) -> Result<(), DeclarationKeyFault> {
    if path.is_empty() {
        return Err(DeclarationKeyFault::Path(DeclarationPathFault::Empty));
    }
    if contains_backslash(path.as_bytes()) {
        return Err(DeclarationKeyFault::Path(DeclarationPathFault::Backslash));
    }
    Ok(())
}

fn validate_callable_anchor(
    anchor: AnonymousCallableAnchor<'_>,
) -> Result<(), TypedDeclarationKeyFault> {
    if anchor.steps.is_empty() {
        return Err(TypedDeclarationKeyFault::EmptyAnchor);
    }
    if anchor.steps.len() > MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS {
        return Err(TypedDeclarationKeyFault::AnchorTooDeep {
            actual: anchor.steps.len(),
            limit: MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS,
        });
    }
    let mut text_bytes = 0_usize;
    for step in anchor.steps {
        let compatible = matches!(
            (step.child_role, step.parent),
            (CallableChildRole::CallArgument, CallableParentShape::Call)
                | (
                    CallableChildRole::VariableInitializer,
                    CallableParentShape::VariableBinding(_)
                )
                | (
                    CallableChildRole::PropertyValue,
                    CallableParentShape::PropertyName(_)
                )
                | (
                    CallableChildRole::ConditionalConsequent
                        | CallableChildRole::ConditionalAlternate,
                    CallableParentShape::Conditional
                )
                | (
                    CallableChildRole::ArrayElement,
                    CallableParentShape::ArrayLiteral
                )
                | (
                    CallableChildRole::ObjectMemberValue,
                    CallableParentShape::ObjectMember(_)
                )
                | (
                    CallableChildRole::SignatureParameterType,
                    CallableParentShape::SignatureParameter(_)
                )
                | (
                    CallableChildRole::TypeExpression,
                    CallableParentShape::TypeContainer(_)
                )
                | (
                    CallableChildRole::TypeAliasValue,
                    CallableParentShape::TypeAliasName(_)
                )
                | (
                    CallableChildRole::CallSignatureMember,
                    CallableParentShape::TypeContainer(
                        CallableTypeContainerKind::TypeLiteral
                            | CallableTypeContainerKind::Interface
                    )
                )
                | (
                    CallableChildRole::ConstructSignatureMember,
                    CallableParentShape::TypeContainer(
                        CallableTypeContainerKind::TypeLiteral
                            | CallableTypeContainerKind::Interface
                    )
                )
        );
        if !compatible {
            return Err(TypedDeclarationKeyFault::RoleShapeMismatch {
                child_role: step.child_role,
                parent_shape: step.parent.tag(),
            });
        }
        match step.parent {
            CallableParentShape::Call => {}
            CallableParentShape::VariableBinding(name)
            | CallableParentShape::PropertyName(name)
            | CallableParentShape::ObjectMember(name)
            | CallableParentShape::SignatureParameter(name)
            | CallableParentShape::TypeAliasName(name)
                if name.is_empty() =>
            {
                return Err(TypedDeclarationKeyFault::EmptyStructuralToken);
            }
            CallableParentShape::VariableBinding(name)
            | CallableParentShape::PropertyName(name)
            | CallableParentShape::ObjectMember(name)
            | CallableParentShape::SignatureParameter(name)
            | CallableParentShape::TypeAliasName(name) => {
                text_bytes = checked_anchor_text_add(text_bytes, name.len())?;
            }
            CallableParentShape::Conditional
            | CallableParentShape::ArrayLiteral
            | CallableParentShape::TypeContainer(_) => {}
        }
    }
    Ok(())
}

fn checked_anchor_text_add(
    accumulated: usize,
    additional: usize,
) -> Result<usize, TypedDeclarationKeyFault> {
    let actual = accumulated.saturating_add(additional);
    if actual > MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES {
        return Err(TypedDeclarationKeyFault::AnchorTextTooLarge {
            actual,
            limit: MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES,
        });
    }
    Ok(actual)
}

fn callable_parent_payload_len(parent: CallableParentShape<'_>) -> Result<usize, PreimageOverflow> {
    match parent {
        CallableParentShape::Call => Ok(0),
        CallableParentShape::VariableBinding(name)
        | CallableParentShape::PropertyName(name)
        | CallableParentShape::ObjectMember(name)
        | CallableParentShape::SignatureParameter(name) => cell_len(name),
        CallableParentShape::TypeAliasName(name) => cell_len(name),
        CallableParentShape::TypeContainer(_) => Ok(1),
        CallableParentShape::Conditional | CallableParentShape::ArrayLiteral => Ok(0),
    }
}

fn callable_role_tag(role: CallableChildRole) -> u8 {
    match role {
        CallableChildRole::CallArgument => 0,
        CallableChildRole::VariableInitializer => 1,
        CallableChildRole::PropertyValue => 2,
        CallableChildRole::ConditionalConsequent => 3,
        CallableChildRole::ConditionalAlternate => 4,
        CallableChildRole::ArrayElement => 5,
        CallableChildRole::ObjectMemberValue => 6,
        CallableChildRole::SignatureParameterType => 7,
        CallableChildRole::TypeExpression => 8,
        CallableChildRole::TypeAliasValue => 9,
        CallableChildRole::CallSignatureMember => 10,
        CallableChildRole::ConstructSignatureMember => 11,
    }
}

fn callable_parent_tag(parent: CallableParentShape<'_>) -> u8 {
    match parent {
        CallableParentShape::Call => 0,
        CallableParentShape::VariableBinding(_) => 1,
        CallableParentShape::PropertyName(_) => 2,
        CallableParentShape::Conditional => 3,
        CallableParentShape::ArrayLiteral => 4,
        CallableParentShape::ObjectMember(_) => 5,
        CallableParentShape::SignatureParameter(_) => 6,
        CallableParentShape::TypeContainer(_) => 7,
        CallableParentShape::TypeAliasName(_) => 8,
    }
}

fn write_callable_parent(
    out: &mut [u8],
    cursor: usize,
    parent: CallableParentShape<'_>,
) -> Result<usize, PreimageOverflow> {
    match parent {
        CallableParentShape::Call => Ok(cursor),
        CallableParentShape::VariableBinding(name)
        | CallableParentShape::PropertyName(name)
        | CallableParentShape::ObjectMember(name)
        | CallableParentShape::SignatureParameter(name)
        | CallableParentShape::TypeAliasName(name) => write_str_cell(out, cursor, name),
        CallableParentShape::TypeContainer(kind) => {
            out[cursor] = kind as u8;
            Ok(cursor + 1)
        }
        CallableParentShape::Conditional | CallableParentShape::ArrayLiteral => Ok(cursor),
    }
}

/// Coordinate-free declaration family identity. It owns only stable scope,
/// profile, parentage, kind, and name; overload instances may share it.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeclarationFamilyId([u8; 16]);

impl DeclarationFamilyId {
    /// Reconstitutes the compact family identity from its raw 16-byte payload.
    /// This does not hash or validate the supplied bytes.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical family preimage in `DeclarationFamilyDomain` and
    /// retains its compact 16-byte identity payload.
    /// Narrows one dedicated declaration-family digest for the compact live
    /// IR lane. The domain remains present in the full content identity at
    /// the mint boundary; this compact value is never a source digest.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let digest = ContentId::<DeclarationFamilyDomain>::from_canonical_bytes(bytes);
        Self::from_content_id(digest)
    }

    /// Narrows an already domain-validated declaration-family content id
    /// without accidentally retaining its wire-domain byte as entropy.
    #[must_use]
    pub fn from_content_id(value: ContentId<DeclarationFamilyDomain>) -> Self {
        Self(compact_identity_payload(value.as_ref()))
    }

    /// Returns the compact 16-byte family payload, without a content-domain tag.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Coordinate-free structural fingerprint for every declaration instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VariantFingerprint([u8; 16]);

impl VariantFingerprint {
    /// Reconstitutes the structural fingerprint from its raw 16-byte payload.
    /// This does not hash or validate the supplied bytes.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical declaration-variant preimage in
    /// `DeclarationVariantDomain` and retains its compact 16-byte payload.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let digest = ContentId::<DeclarationVariantDomain>::from_canonical_bytes(bytes);
        Self(compact_identity_payload(digest.as_ref()))
    }

    /// Returns the compact 16-byte structural fingerprint payload.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Compact unresolved foreign declaration key. It is deliberately not a
/// [`DeclarationFamilyId`]: a foreign authority key cannot be substituted
/// for a locally minted lexical declaration family.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ForeignDeclarationId([u8; 16]);

impl ForeignDeclarationId {
    /// Reconstitutes the unresolved foreign-key fingerprint from its raw
    /// 16-byte payload, without deriving or validating it.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical foreign-key preimage in `ForeignDeclarationDomain`
    /// and retains its compact 16-byte fingerprint.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self::from_content_id(ContentId::<ForeignDeclarationDomain>::from_canonical_bytes(
            bytes,
        ))
    }

    /// Narrows a foreign-key content id without mixing its wire domain byte
    /// into the compact fingerprint.
    #[must_use]
    pub fn from_content_id(value: ContentId<ForeignDeclarationDomain>) -> Self {
        Self(compact_identity_payload(value.as_ref()))
    }

    /// Returns the compact 16-byte foreign-key fingerprint payload.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact current-generation local declaration endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeclarationIdentity {
    /// Stable declaration family shared by structurally distinct instances such as overloads.
    pub family: DeclarationFamilyId,
    /// Structural fingerprint distinguishing this exact declaration instance within its family.
    pub variant: VariantFingerprint,
}

/// Variant knowledge retained for an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VariantAvailability {
    /// The foreign target's exact structural declaration fingerprint is known.
    Known(VariantFingerprint),
    /// The target is unresolved and no variant fingerprint was available to record.
    Unavailable,
}

/// Cross-package declaration identity without fabricating a local variant.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternalDeclarationIdentity {
    /// Exact unresolved foreign-key fingerprint, never a local family.
    pub foreign: ForeignDeclarationId,
    /// Known target variant, or an explicit marker that its variant is unavailable.
    pub variant: VariantAvailability,
}

/// Wire-stable cross-fragment declaration reference: the owning fragment's
/// typed artifact identity plus the target's exact composite declaration identity.
///
/// This is the **only** cross-fragment reference form. Both cells are
/// independently valid typed identities; containment (the entity id naming a
/// declaration inside that fragment) is proven at resolution time by the
/// owning fragment, not at construction — references are honest data that a
/// resolver validates.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableRef {
    /// Typed identity of the fragment that declares the target.
    pub fragment: crate::ir_vocabulary::ExternalFragmentId,
    /// Exact declaration endpoint inside that fragment.
    pub declaration: DeclarationIdentity,
}

impl StableRef {
    /// Complete canonical byte width: one 32-byte fragment identity plus
    /// two compact 16-byte declaration identity cells.
    pub const CANONICAL_BYTES: usize = 64;
}

const _: () = assert!(size_of::<StableRef>() == StableRef::CANONICAL_BYTES);

/// Where an unresolved foreign target lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForeignOrigin<'bytes> {
    /// Another loaded package lineage.
    Package(PackageLineage<'bytes>),
    /// A namespace outside any package lineage (`java.util`,
    /// assembly-qualified .NET namespaces).
    Namespace {
        /// Ecosystem registry name.
        ecosystem: &'bytes str,
        /// Namespace spelling.
        namespace: &'bytes str,
    },
    /// A universe scope outside every package (`error`, `comparable` in Go;
    /// C builtins).
    Universe {
        /// Ecosystem registry name.
        ecosystem: &'bytes str,
    },
}

/// Ecosystem discriminator for a TypeScript reference whose target is known
/// by a source coordinate in the same admitted TSZ program. The external
/// target remains a [`ForeignKey`] in the canonical occurrence lane, while
/// project query joins can distinguish this typed coordinate from a package
/// or a name-only universe reference.
pub const TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM: &str = "typescript-tsz-source-v1";

/// Exact declaration coordinate proven by native Python in one selected source frontier.
pub const PYTHON_NATIVE_SOURCE_ECOSYSTEM: &str = "python-native-source-v1";

/// Native Python definition evidence reuses the admitted source-coordinate cells.
/// It carries a distinct producer grammar and never resolves by module/name guessing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PythonSourceCoordinate<'source>(pub SourceDeclarationCoordinate<'source>);

impl<'source> PythonSourceCoordinate<'source> {
    const PREFIX: &'static str = "python-native-source-coordinate-v1:";

    /// Encodes only canonical relative selected-source paths and admitted extents.
    #[must_use]
    pub fn encode(self) -> Option<String> {
        if !python_source_path(self.0.path) {
            return None;
        }
        self.0.encode_with_prefix(Self::PREFIX)
    }

    /// Decodes the closed Python producer grammar without a TSZ authority promotion.
    #[must_use]
    pub fn decode(encoded: &'source str) -> Option<Self> {
        let coordinate = TypeScriptSourceCoordinate::decode_with_prefix(encoded, Self::PREFIX)?;
        let result = Self(coordinate);
        (python_source_path(coordinate.path) && result.encode().as_deref() == Some(encoded))
            .then_some(result)
    }
}

fn python_source_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Shared exact declaration source-coordinate cells. Producer-specific codecs
/// retain their own authority discriminator; these cells alone imply no authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceDeclarationCoordinate<'source> {
    /// Deterministic identity of the complete admitted project source set.
    pub program: [u8; 32],
    /// Content identity of the target source file in that project.
    pub source: [u8; 32],
    /// Stable project-relative source path supplied to the native producer.
    pub path: &'source str,
    /// Start of the exact target declaration span in UTF-8 bytes.
    pub declaration_start: u32,
    /// End of the exact target declaration span in UTF-8 bytes.
    pub declaration_end: u32,
    /// Byte offset of the declaration name token in that exact file.
    pub name_start: u32,
}

/// Existing TSZ source-coordinate API and codec remain byte-compatible.
pub type TypeScriptSourceCoordinate<'source> = SourceDeclarationCoordinate<'source>;

/// Authenticated current source site of one anonymous TypeScript callable.
/// The program and source digests prove where the producer observed the site;
/// current instance identity is based on the stable family and byte range,
/// so same-length body edits keep the instance endpoint stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeScriptCallableSourceCoordinate<'source> {
    /// Deterministic identity of the exact admitted project source set.
    program: [u8; 32],
    /// Content identity of the file in that exact source set.
    source: [u8; 32],
    /// Stable project-relative source path supplied to the TSZ project.
    path: &'source str,
    /// Start of the exact anonymous callable span in UTF-8 bytes.
    callable_start: u32,
    /// End of the exact anonymous callable span in UTF-8 bytes.
    callable_end: u32,
}

/// Exact malformed anonymous callable source-site witness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeScriptCallableCoordinateFault {
    /// The project-relative source path is empty.
    EmptyPath,
    /// The path contains a backslash instead of canonical `/` separators.
    BackslashInPath,
    /// The path contains a NUL byte.
    NulInPath,
    /// The callable source range is empty or reversed.
    EmptyOrReversedSpan,
}

impl<'source> TypeScriptCallableSourceCoordinate<'source> {
    /// Validates the exact TSZ project/file source site of an anonymous
    /// callable. The authority must separately verify the digests and range
    /// against the retained project input before admitting this value.
    pub fn new(
        program: [u8; 32],
        source: [u8; 32],
        path: &'source str,
        callable_start: u32,
        callable_end: u32,
    ) -> Result<Self, TypeScriptCallableCoordinateFault> {
        if path.is_empty() {
            return Err(TypeScriptCallableCoordinateFault::EmptyPath);
        }
        if path.contains('\\') {
            return Err(TypeScriptCallableCoordinateFault::BackslashInPath);
        }
        if path.contains('\0') {
            return Err(TypeScriptCallableCoordinateFault::NulInPath);
        }
        if callable_start >= callable_end {
            return Err(TypeScriptCallableCoordinateFault::EmptyOrReversedSpan);
        }
        Ok(Self {
            program,
            source,
            path,
            callable_start,
            callable_end,
        })
    }

    /// Returns the exact TSZ project identity that admitted this source.
    #[must_use]
    pub const fn program(self) -> [u8; 32] {
        self.program
    }

    /// Returns the exact source-file content identity.
    #[must_use]
    pub const fn source(self) -> [u8; 32] {
        self.source
    }

    /// Returns the canonical project-relative source path.
    #[must_use]
    pub const fn path(self) -> &'source str {
        self.path
    }

    /// Returns the start byte of the exact anonymous callable syntax node.
    #[must_use]
    pub const fn callable_start(self) -> u32 {
        self.callable_start
    }

    /// Returns the end byte of the exact anonymous callable syntax node.
    #[must_use]
    pub const fn callable_end(self) -> u32 {
        self.callable_end
    }
}

impl<'source> SourceDeclarationCoordinate<'source> {
    const PREFIX: &'static str = "tsz-source-coordinate-v2:";

    /// Encodes the existing TSZ producer grammar into the validated foreign-key path
    /// cell. Other producers use their distinct wrapper codec. A byte-length prefix keeps paths containing colons or Unicode
    /// unambiguous without introducing a second persisted operand.
    #[must_use]
    pub fn encode(self) -> Option<String> {
        self.encode_with_prefix(Self::PREFIX)
    }

    fn encode_with_prefix(self, prefix: &str) -> Option<String> {
        if self.path.is_empty()
            || self.path.contains('\\')
            || self.path.contains('\0')
            || self.declaration_start >= self.declaration_end
            || self.name_start < self.declaration_start
            || self.name_start >= self.declaration_end
        {
            return None;
        }
        Some(format!(
            "{}{}:{}:{}:{}:{}:{}:{}",
            prefix,
            hex_digest(&self.program),
            hex_digest(&self.source),
            self.path.len(),
            self.path,
            self.declaration_start,
            self.declaration_end,
            self.name_start
        ))
    }

    /// Decodes only the existing closed TSZ source-coordinate grammar.
    #[must_use]
    pub fn decode(encoded: &'source str) -> Option<Self> {
        Self::decode_with_prefix(encoded, Self::PREFIX)
    }

    fn decode_with_prefix(encoded: &'source str, prefix: &str) -> Option<Self> {
        let rest = encoded.strip_prefix(prefix)?;
        let (program, rest) = rest.split_once(':')?;
        let program = parse_hex_digest(program)?;
        let (source, rest) = rest.split_once(':')?;
        let source = parse_hex_digest(source)?;
        let (path_len, rest) = rest.split_once(':')?;
        let path_len = path_len.parse::<usize>().ok()?;
        let path = rest.get(..path_len)?;
        let (declaration_start, rest) = rest.get(path_len..)?.strip_prefix(':')?.split_once(':')?;
        let (declaration_end, name_start) = rest.split_once(':')?;
        let declaration_start = declaration_start.parse().ok()?;
        let declaration_end = declaration_end.parse().ok()?;
        let name_start = name_start.parse().ok()?;
        if path.is_empty()
            || path.contains('\\')
            || path.contains('\0')
            || declaration_start >= declaration_end
            || name_start < declaration_start
            || name_start >= declaration_end
        {
            return None;
        }
        Some(Self {
            program,
            source,
            path,
            declaration_start,
            declaration_end,
            name_start,
        })
    }
}

/// Hashes one exact project source manifest. Duplicate paths are rejected,
/// rather than allowing source order or accidental last-writer wins to
/// decide which program a cross-file reference names.
#[must_use]
pub fn typescript_program_identity(sources: &[(String, [u8; 32])]) -> Option<[u8; 32]> {
    source_manifest_identity(b"typescript-tsz-program-source-manifest-v1\0", sources)
}

/// Binds native Python targets to the exact selected source frontier.
#[must_use]
pub fn python_program_identity(sources: &[(String, [u8; 32])]) -> Option<[u8; 32]> {
    if sources.iter().any(|(path, _)| !python_source_path(path)) {
        return None;
    }
    source_manifest_identity(b"python-native-program-source-manifest-v1\0", sources)
}

fn source_manifest_identity(domain: &[u8], sources: &[(String, [u8; 32])]) -> Option<[u8; 32]> {
    if sources.is_empty() {
        return None;
    }
    let mut ordered = sources.to_vec();
    ordered.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for (path, source) in ordered {
        let length = u32::try_from(path.len()).ok()?;
        hasher.update(&length.to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&source);
    }
    Some(*hasher.finalize().as_bytes())
}

fn hex_digest(digest: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest {
        use core::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_hex_digest(digest: &str) -> Option<[u8; 32]> {
    if digest.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = u8::from_str_radix(digest.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    if hex_digest(&decoded) != digest {
        return None;
    }
    Some(decoded)
}

impl<'bytes> ForeignOrigin<'bytes> {
    /// Stable wire tag of the [`ForeignOrigin::Package`] family.
    pub const PACKAGE_TAG: u8 = 0;
    /// Stable wire tag of the [`ForeignOrigin::Namespace`] family.
    pub const NAMESPACE_TAG: u8 = 1;
    /// Stable wire tag of the [`ForeignOrigin::Universe`] family.
    pub const UNIVERSE_TAG: u8 = 2;

    /// The stable preimage tag of this origin.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Package(_) => Self::PACKAGE_TAG,
            Self::Namespace { .. } => Self::NAMESPACE_TAG,
            Self::Universe { .. } => Self::UNIVERSE_TAG,
        }
    }
}

/// Everything a producer knows about a foreign target at the reference site.
///
/// `display` is exactly what renders for an unlinked reference; it is
/// deliberately **excluded from the key digest** so sealing with and without
/// dependencies loaded stays byte-identical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForeignKey<'bytes> {
    /// Where the target lives.
    pub origin: ForeignOrigin<'bytes>,
    /// Canonical cross-package path of the target.
    pub path: &'bytes str,
    /// Human display spelling at the reference site.
    pub display: &'bytes str,
    /// Expected declaration kind, when the producer knows one.
    pub kind: Option<EntityKind>,
}

/// Exact foreign-key rejection retaining the offending operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForeignKeyFault {
    /// The canonical path is empty.
    EmptyPath,
    /// The canonical path contains a backslash; callers normalize to `/`.
    BackslashInPath,
}

impl<'bytes> ForeignKey<'bytes> {
    /// Validates one foreign key: the canonical path is non-empty and
    /// `/`-separated. `display` may be empty (the producer may honestly know
    /// nothing worth displaying beyond the path).
    pub const fn new(
        origin: ForeignOrigin<'bytes>,
        path: &'bytes str,
        display: &'bytes str,
        kind: Option<EntityKind>,
    ) -> Result<Self, ForeignKeyFault> {
        if path.is_empty() {
            return Err(ForeignKeyFault::EmptyPath);
        }
        if contains_backslash(path.as_bytes()) {
            return Err(ForeignKeyFault::BackslashInPath);
        }
        Ok(Self {
            origin,
            path,
            display,
            kind,
        })
    }

    /// Complete canonical key-digest preimage length.
    pub fn key_preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let mut length = cell_len(FOREIGN_KEY_PURPOSE)?;
        length = add_preimage_len(length, ORIGIN_CELL)?;
        match self.origin {
            ForeignOrigin::Package(lineage) => {
                length = add_preimage_len(length, cell_len(lineage.ecosystem.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(lineage.name.as_bytes())?)?;
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                length = add_preimage_len(length, cell_len(ecosystem.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(namespace.as_bytes())?)?;
            }
            ForeignOrigin::Universe { ecosystem } => {
                length = add_preimage_len(length, cell_len(ecosystem.as_bytes())?)?;
            }
        }
        add_preimage_len(length, cell_len(self.path.as_bytes())?)
    }

    /// Writes the canonical key-digest preimage, excluding `display`.
    /// Output is written only after the total-length preflight.
    pub fn write_key_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.key_preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let mut cursor = 0;
        cursor = write_str_cell(out, cursor, FOREIGN_KEY_PURPOSE)?;
        // Fixed origin cell: origin tag, kind cell biased by one so an
        // unknown kind (`None`) and kind `Function` never share a cell.
        let kind_cell = match self.kind {
            None => 0_u16,
            Some(kind) => u16::from(kind) + 1,
        };
        let mut origin = [0_u8; ORIGIN_CELL];
        origin[0] = self.origin.tag();
        origin[1..3].copy_from_slice(&kind_cell.to_le_bytes());
        out[cursor..cursor + ORIGIN_CELL].copy_from_slice(&origin);
        cursor += ORIGIN_CELL;
        cursor = match self.origin {
            ForeignOrigin::Package(lineage) => {
                let after_ecosystem = write_str_cell(out, cursor, lineage.ecosystem.as_bytes())?;
                write_str_cell(out, after_ecosystem, lineage.name.as_bytes())?
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                let after_ecosystem = write_str_cell(out, cursor, ecosystem.as_bytes())?;
                write_str_cell(out, after_ecosystem, namespace.as_bytes())?
            }
            ForeignOrigin::Universe { ecosystem } => {
                write_str_cell(out, cursor, ecosystem.as_bytes())?
            }
        };
        cursor = write_str_cell(out, cursor, self.path.as_bytes())?;
        Ok(cursor)
    }

    /// Digests the key cells (never a resolved target, never `display`)
    /// through the foreign-declaration identity domain, so sealing with and without
    /// dependencies loaded is byte-identical.
    pub fn key_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<ForeignDeclarationDomain>, PreimageOverflow> {
        let written = self.write_key_preimage(out)?;
        Ok(ContentId::<ForeignDeclarationDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

/// The outcome of resolving one [`ForeignKey`] against loaded dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// The target resolved to a stable declaration reference.
    Resolved(StableRef),
    /// The owning package is not loaded; the key remains self-describing.
    PackageNotLoaded,
    /// The package is loaded but names no such path.
    PathNotFound,
}

/// One reference fact's target: resolved, or a self-describing foreign key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccurrenceTarget<'bytes> {
    /// A resolved cross-fragment declaration reference.
    Stable(StableRef),
    /// The producer could not resolve the target; the key travels instead.
    Foreign(ForeignKey<'bytes>),
    /// A declaration inside the same fragment as the reference.
    ///
    /// Same-fragment references never embed their own artifact identity: a
    /// fragment's content identity is derived from its written bytes, so a
    /// [`StableRef`](StableRef) naming the carrying fragment cannot appear
    /// inside those bytes. The local ordinal is proven against the entity
    /// lane by whichever lane admits the occurrence.
    Local(EntityId),
}

/// One reference fact: the owning declaration (implied by the containing
/// lane) references `target` via `kind` at `span`, resolved at
/// `confidence`.
///
/// The owner is not a field; it is implied by whichever lane holds this
/// value, keeping the record compact.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occurrence<'bytes> {
    /// The referenced declaration.
    pub target: OccurrenceTarget<'bytes>,
    /// The category of this reference.
    pub kind: crate::ir_vocabulary::occurrence::ReferenceKind,
    /// The fidelity of the resolution.
    pub confidence: crate::ir_vocabulary::occurrence::Confidence,
    /// The byte range of the reference site, relative to the owning
    /// declaration's span start.
    pub span: crate::ir_vocabulary::occurrence::RelSpan,
}

/// Checked byte width of one length-prefixed string cell.
fn cell_len(bytes: &[u8]) -> Result<usize, PreimageOverflow> {
    if u32::try_from(bytes.len()).is_err() {
        return Err(PreimageOverflow::CellTooLong {
            actual: bytes.len(),
        });
    }
    add_preimage_len(4, bytes.len())
}

/// Adds one already-validated canonical segment without allowing platform
/// width wraparound to become an undersized caller allocation.
fn add_preimage_len(accumulated: usize, additional: usize) -> Result<usize, PreimageOverflow> {
    accumulated
        .checked_add(additional)
        .ok_or(PreimageOverflow::AggregateTooLong {
            accumulated,
            additional,
        })
}
/// Byte width of the family-only fixed key tail: kind u16.
const KEY_TAIL_BYTES: usize = 2;
/// Byte width of the fixed origin cell: origin tag u8 + kind cell u16 +
/// reserved u8.
const ORIGIN_CELL: usize = 4;

/// Narrows sixteen payload bytes from a typed 32-byte content identity.
/// Byte zero is the domain authority, not digest entropy, so compact
/// identities retain bytes `1..=16` rather than spending one of their fixed
/// sixteen bytes on a constant tag.
fn compact_identity_payload(bytes: &[u8; backend_version::HASH_BYTES]) -> [u8; 16] {
    [
        bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16],
    ]
}

/// Writes one length-prefixed byte cell at `cursor`; returns the next
/// cursor. The caller preflighted the total length, so the bounds checks
/// below are proven in bounds; a failed check names the exact shortfall.
/// Writes one length-prefixed canonical byte cell into a caller-sized buffer.
pub(crate) fn write_str_cell(
    out: &mut [u8],
    cursor: usize,
    bytes: &[u8],
) -> Result<usize, PreimageOverflow> {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return Err(PreimageOverflow::CellTooLong {
            actual: bytes.len(),
        });
    };
    let after = cursor
        .checked_add(4)
        .and_then(|cursor| cursor.checked_add(bytes.len()))
        .ok_or(PreimageOverflow::OutputShort {
            needed: usize::MAX,
            actual: out.len(),
        })?;
    let out_len = out.len();
    let cell = out
        .get_mut(cursor..after)
        .ok_or(PreimageOverflow::OutputShort {
            needed: after,
            actual: out_len,
        })?;
    let (length_cell, payload) = cell.split_at_mut(4);
    length_cell.copy_from_slice(&len.to_le_bytes());
    payload.copy_from_slice(bytes);
    Ok(after)
}

#[cfg(test)]
mod typescript_source_coordinate_tests {
    use super::{TypeScriptSourceCoordinate, typescript_program_identity};

    #[test]
    fn source_coordinate_round_trips_colons_and_utf8_by_byte_length() {
        let path = "src/über:service.ts";
        let encoded = TypeScriptSourceCoordinate {
            program: [4; 32],
            source: [5; 32],
            path,
            declaration_start: 30,
            declaration_end: 41,
            name_start: 37,
        }
        .encode()
        .expect("valid source coordinate");
        assert_eq!(
            TypeScriptSourceCoordinate::decode(&encoded),
            Some(TypeScriptSourceCoordinate {
                program: [4; 32],
                source: [5; 32],
                path,
                declaration_start: 30,
                declaration_end: 41,
                name_start: 37,
            })
        );
    }

    #[test]
    fn source_coordinate_rejects_noncanonical_paths_and_malformed_lengths() {
        assert!(
            TypeScriptSourceCoordinate {
                program: [4; 32],
                source: [5; 32],
                path: "src\\service.ts",
                declaration_start: 0,
                declaration_end: 1,
                name_start: 1,
            }
            .encode()
            .is_none()
        );
        assert!(TypeScriptSourceCoordinate::decode("tsz-source-coordinate-v2:999:a:1").is_none());
        assert!(TypeScriptSourceCoordinate::decode("tsz-source-coordinate-v2:3:a:b:1").is_none());
    }

    #[test]
    fn project_identity_commits_sorted_complete_sources_and_rejects_duplicate_paths() {
        let forward = typescript_program_identity(&[
            ("src/b.ts".to_owned(), [2; 32]),
            ("src/a.ts".to_owned(), [1; 32]),
        ]);
        let reverse = typescript_program_identity(&[
            ("src/a.ts".to_owned(), [1; 32]),
            ("src/b.ts".to_owned(), [2; 32]),
        ]);
        assert_eq!(forward, reverse);
        assert_ne!(
            forward,
            typescript_program_identity(&[("src/a.ts".to_owned(), [1; 32])])
        );
        assert!(
            typescript_program_identity(&[
                ("src/a.ts".to_owned(), [1; 32]),
                ("src/a.ts".to_owned(), [1; 32])
            ])
            .is_none()
        );
    }
}

#[cfg(test)]
mod python_source_coordinate_tests {
    use super::{
        PythonSourceCoordinate, SourceDeclarationCoordinate, TypeScriptSourceCoordinate,
        python_program_identity, typescript_program_identity,
    };

    #[test]
    fn python_coordinate_producer_and_source_frontier_are_distinct_and_canonical()
    -> Result<(), &'static str> {
        let sources = [
            ("core/api/utils.py".to_owned(), [7; 32]),
            ("core/api/viewsets.py".to_owned(), [8; 32]),
        ];
        let program = python_program_identity(&sources).ok_or("source manifest")?;
        assert_eq!(
            Some(program),
            python_program_identity(&[sources[1].clone(), sources[0].clone()])
        );
        assert_ne!(Some(program), typescript_program_identity(&sources));
        assert_eq!(
            python_program_identity(&[sources[0].clone(), sources[0].clone()]),
            None
        );
        assert_eq!(
            python_program_identity(&[("../utils.py".to_owned(), [7; 32])]),
            None
        );
        let cells = SourceDeclarationCoordinate {
            program,
            source: [7; 32],
            path: "core/api/utils.py",
            declaration_start: 70,
            declaration_end: 100,
            name_start: 74,
        };
        let encoded = PythonSourceCoordinate(cells).encode().ok_or("coordinate")?;
        assert_eq!(
            PythonSourceCoordinate::decode(&encoded),
            Some(PythonSourceCoordinate(cells))
        );
        assert_eq!(TypeScriptSourceCoordinate::decode(&encoded), None);
        let tsz = cells.encode().ok_or("existing TSZ coordinate")?;
        assert!(tsz.starts_with("tsz-source-coordinate-v2:"));
        assert_eq!(PythonSourceCoordinate::decode(&tsz), None);
        let noncanonical = encoded.replace(":70:100:74", ":070:100:74");
        assert_eq!(PythonSourceCoordinate::decode(&noncanonical), None);
        for path in [
            "/utils.py",
            "core//utils.py",
            "core/./utils.py",
            "core/../utils.py",
            "C:\\utils.py",
        ] {
            assert_eq!(
                PythonSourceCoordinate(SourceDeclarationCoordinate { path, ..cells }).encode(),
                None
            );
        }
        assert_ne!(
            Some(program),
            python_program_identity(&[(sources[0].0.clone(), [9; 32]), sources[1].clone()])
        );
        Ok(())
    }
}
