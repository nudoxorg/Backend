//! Recursive semantic products and their typed local or external references.
//! Products retain constructor roles independently of source-language spellings.
//! The compact fragment wire uses these values without creating a second ID system.

use core::marker::PhantomData;

use heart_identity::{ContentId, IrFragmentDomain};

use crate::{AtomId, DenseId, Entity, Type};

/// Marker for a recursive semantic-product coordinate in one fragment.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Product {}

/// Marker for a pooled list of semantic-product children.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProductChildren {}

/// Dense coordinate of a recursive semantic product inside one IR fragment.
pub type ProductId = DenseId<Product>;
/// Dense coordinate of one product's pooled child-list row.
pub type ProductListId = DenseId<ProductChildren>;

/// A borrowed semantic atom. The caller retains the bytes until canonical
/// emission copies the selected value into its output region.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SemanticAtom<'bytes> {
    /// Exact atom bytes retained by the caller.
    pub bytes: &'bytes [u8],
}

impl AsRef<[u8]> for SemanticAtom<'_> {
    /// Borrows the exact atom bytes without a copy.
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}

/// The central typed authority that gives an external product ordinal meaning.
pub type ExternalFragmentId = ContentId<IrFragmentDomain>;

/// A kind-owned coordinate inside one external fragment authority.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExternalCoordinate<ExpectedKind> {
    /// Typed fragment authority that owns `ordinal`.
    pub fragment: ExternalFragmentId,
    /// Dense position inside the external fragment's kind-owned space.
    pub ordinal: u32,
    expected_kind: PhantomData<fn() -> ExpectedKind>,
}

impl<ExpectedKind> ExternalCoordinate<ExpectedKind> {
    /// Binds one external ordinal to its fragment authority and expected kind.
    #[must_use]
    pub const fn bind(fragment: ExternalFragmentId, ordinal: u32) -> Self {
        Self {
            fragment,
            ordinal,
            expected_kind: PhantomData,
        }
    }
}

/// External authority for an entity coordinate.
pub type ExternalEntityRef = ExternalCoordinate<Entity>;
/// External authority for a semantic-type coordinate.
pub type ExternalTypeRef = ExternalCoordinate<Type>;
/// External authority for a recursive semantic-product coordinate.
pub type ExternalProductRef = ExternalCoordinate<Product>;

/// One recursive product child. Foreign ordinals remain inseparable from
/// their typed fragment authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProductRef {
    /// A product inside the same fragment.
    Local(ProductId),
    /// A product owned by another fragment authority.
    External(ExternalProductRef),
}

/// Closed semantic role of one ordered product child.
///
/// The role travels with the child rather than being reconstructed from a
/// product-head spelling or an untyped list offset. A formatter may use the
/// enclosing constructor to validate the role sequence, but cannot silently
/// erase the distinction between, for example, a function parameter and its
/// result.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProductChildRole {
    /// An ordered function parameter.
    FunctionParameter = 0,
    /// An ordered function result.
    FunctionResult = 1,
    /// An ordered generic argument.
    GenericArgument = 2,
    /// An ordered tuple element.
    TupleElement = 3,
    /// The sole element type of an array constructor.
    ArrayElement = 4,
    /// An ordered union member.
    UnionMember = 5,
    /// An ordered intersection member.
    IntersectionMember = 6,
    /// A language-owned product member.
    ProductMember = 7,
}

/// One product child whose local or external target and semantic role remain
/// inseparable through canonical lowering.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SemanticProductChild {
    /// Kind-safe local or external target.
    pub target: ProductRef,
    /// Semantic role independent of source spelling and position.
    pub role: ProductChildRole,
}

/// A product-kind-owned range in a caller-owned pooled child lane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListSpan<Owner> {
    /// First pooled position owned by this list.
    pub start: u32,
    /// Pooled position count owned by this list.
    pub length: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> ListSpan<Owner> {
    /// Creates a span from a caller-proved pooled range.
    #[must_use]
    pub const fn new(start: u32, length: u32) -> Self {
        Self {
            start,
            length,
            owner: PhantomData,
        }
    }
}

/// Exact pooled-list rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PooledListError {
    /// The requested span is outside the caller's pooled lane.
    OutOfBounds {
        /// Requested first position.
        start: u32,
        /// Requested length.
        length: u32,
        /// Complete pool length.
        pool_length: usize,
    },
}

/// Validated zero-allocation projection of one product-child list.
pub struct ProductList<'pool> {
    elements: &'pool [SemanticProductChild],
}

impl<'pool> ProductList<'pool> {
    /// Validates one pooled span against its caller-owned child pool.
    pub fn try_from_parts(
        pool: &'pool [SemanticProductChild],
        span: ListSpan<ProductChildren>,
    ) -> Result<Self, PooledListError> {
        let start = usize::try_from(span.start).map_err(|_| PooledListError::OutOfBounds {
            start: span.start,
            length: span.length,
            pool_length: pool.len(),
        })?;
        let length = usize::try_from(span.length).map_err(|_| PooledListError::OutOfBounds {
            start: span.start,
            length: span.length,
            pool_length: pool.len(),
        })?;
        let Some(end) = start.checked_add(length) else {
            return Err(PooledListError::OutOfBounds {
                start: span.start,
                length: span.length,
                pool_length: pool.len(),
            });
        };
        let Some(elements) = pool.get(start..end) else {
            return Err(PooledListError::OutOfBounds {
                start: span.start,
                length: span.length,
                pool_length: pool.len(),
            });
        };
        Ok(Self { elements })
    }

    /// Borrows the validated child range without a copy.
    #[must_use]
    pub const fn as_slice(&self) -> &'pool [SemanticProductChild] {
        self.elements
    }
}

impl AsRef<[SemanticProductChild]> for ProductList<'_> {
    /// Borrows the validated child range for generic slice consumers.
    fn as_ref(&self) -> &[SemanticProductChild] {
        self.elements
    }
}

/// One recursive semantic product with a pooled child-list coordinate.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticProduct {
    /// Atom coordinate of the product head.
    pub head: AtomId,
    /// Pooled child-list coordinate.
    pub children: ProductListId,
}

/// Closed structural meaning for one semantic product.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProductConstructorTag {
    /// A function constructor with parameter and result children.
    Function = 0,
    /// A generic constructor with argument children.
    Generic = 1,
    /// A tuple constructor with element children.
    Tuple = 2,
    /// An array constructor with exactly one element child.
    Array = 3,
    /// A union constructor with member children.
    Union = 4,
    /// An intersection constructor with member children.
    Intersection = 5,
    /// A language-owned product constructor with member children.
    Product = 6,
}

impl From<ProductConstructorTag> for u32 {
    /// Encodes the stable constructor-tag discriminant.
    fn from(value: ProductConstructorTag) -> Self {
        match value {
            ProductConstructorTag::Function => 0,
            ProductConstructorTag::Generic => 1,
            ProductConstructorTag::Tuple => 2,
            ProductConstructorTag::Array => 3,
            ProductConstructorTag::Union => 4,
            ProductConstructorTag::Intersection => 5,
            ProductConstructorTag::Product => 6,
        }
    }
}

impl TryFrom<u32> for ProductConstructorTag {
    type Error = ProductConstructorFault;

    /// Decodes a stable constructor-tag discriminant, rejecting unknown values
    /// with the exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Function),
            1 => Ok(Self::Generic),
            2 => Ok(Self::Tuple),
            3 => Ok(Self::Array),
            4 => Ok(Self::Union),
            5 => Ok(Self::Intersection),
            6 => Ok(Self::Product),
            actual => Err(ProductConstructorFault::Tag { actual }),
        }
    }
}

/// Product-aligned constructor payload. Function rows retain their
/// parameter/result boundary; generic and array rows retain their cardinality.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SemanticProductConstructor {
    /// Closed structural tag.
    pub tag: ProductConstructorTag,
    /// First tag-owned payload cell.
    pub payload0: u32,
    /// Second tag-owned payload cell.
    pub payload1: u32,
}

impl SemanticProductConstructor {
    /// Creates the function constructor with its parameter and result counts.
    #[must_use]
    pub const fn function(parameter_count: u32, result_count: u32) -> Self {
        Self {
            tag: ProductConstructorTag::Function,
            payload0: parameter_count,
            payload1: result_count,
        }
    }

    /// Creates the generic constructor with its argument count.
    #[must_use]
    pub const fn generic(argument_count: u32) -> Self {
        Self {
            tag: ProductConstructorTag::Generic,
            payload0: argument_count,
            payload1: 0,
        }
    }

    /// The tuple constructor payload.
    pub const TUPLE: Self = Self::plain(ProductConstructorTag::Tuple);
    /// The union constructor payload.
    pub const UNION: Self = Self::plain(ProductConstructorTag::Union);
    /// The intersection constructor payload.
    pub const INTERSECTION: Self = Self::plain(ProductConstructorTag::Intersection);
    /// The language-owned product constructor payload.
    pub const PRODUCT: Self = Self::plain(ProductConstructorTag::Product);

    /// Creates the array constructor with its element-type length cell.
    #[must_use]
    pub const fn array(length: u32) -> Self {
        Self {
            tag: ProductConstructorTag::Array,
            payload0: length,
            payload1: 0,
        }
    }

    const fn plain(tag: ProductConstructorTag) -> Self {
        Self {
            tag,
            payload0: 0,
            payload1: 0,
        }
    }

    /// Validates raw constructor cells against the child count they claim.
    pub fn try_from_parts(
        tag: u32,
        payload0: u32,
        payload1: u32,
        child_count: u32,
    ) -> Result<Self, ProductConstructorFault> {
        let constructor = Self {
            tag: ProductConstructorTag::try_from(tag)?,
            payload0,
            payload1,
        };
        constructor.validate(child_count)?;
        Ok(constructor)
    }

    /// Proves one constructor payload against its exact child count.
    pub fn validate(self, child_count: u32) -> Result<(), ProductConstructorFault> {
        let expected = match self.tag {
            ProductConstructorTag::Function => self.payload0.checked_add(self.payload1).ok_or(
                ProductConstructorFault::ArityOverflow {
                    tag: self.tag,
                    payload0: self.payload0,
                    payload1: self.payload1,
                },
            )?,
            ProductConstructorTag::Generic => {
                self.require_second_zero()?;
                self.payload0
            }
            ProductConstructorTag::Array => {
                self.require_second_zero()?;
                1
            }
            ProductConstructorTag::Tuple
            | ProductConstructorTag::Union
            | ProductConstructorTag::Intersection
            | ProductConstructorTag::Product => {
                if self.payload0 != 0 || self.payload1 != 0 {
                    return Err(ProductConstructorFault::ReservedPayload {
                        tag: self.tag,
                        payload0: self.payload0,
                        payload1: self.payload1,
                    });
                }
                return Ok(());
            }
        };
        if expected != child_count {
            return Err(ProductConstructorFault::Arity {
                tag: self.tag,
                expected,
                actual: child_count,
            });
        }
        Ok(())
    }

    fn require_second_zero(self) -> Result<(), ProductConstructorFault> {
        if self.payload1 == 0 {
            Ok(())
        } else {
            Err(ProductConstructorFault::ReservedPayload {
                tag: self.tag,
                payload0: self.payload0,
                payload1: self.payload1,
            })
        }
    }

    /// The closed child role each ordered position must carry under this
    /// constructor. One declaration serves input validation and trusted
    /// wire validation.
    #[must_use]
    pub const fn expected_role(self, position: u32) -> ProductChildRole {
        match self.tag {
            ProductConstructorTag::Function => {
                if position < self.payload0 {
                    ProductChildRole::FunctionParameter
                } else {
                    ProductChildRole::FunctionResult
                }
            }
            ProductConstructorTag::Generic => ProductChildRole::GenericArgument,
            ProductConstructorTag::Tuple => ProductChildRole::TupleElement,
            ProductConstructorTag::Array => ProductChildRole::ArrayElement,
            ProductConstructorTag::Union => ProductChildRole::UnionMember,
            ProductConstructorTag::Intersection => ProductChildRole::IntersectionMember,
            ProductConstructorTag::Product => ProductChildRole::ProductMember,
        }
    }
}

/// Exact role-code rejection retaining the observed byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductChildRoleCodeError {
    /// Rejected role byte.
    pub actual: u8,
}

impl From<ProductChildRole> for u8 {
    /// Encodes the stable role discriminant.
    fn from(value: ProductChildRole) -> Self {
        match value {
            ProductChildRole::FunctionParameter => 0,
            ProductChildRole::FunctionResult => 1,
            ProductChildRole::GenericArgument => 2,
            ProductChildRole::TupleElement => 3,
            ProductChildRole::ArrayElement => 4,
            ProductChildRole::UnionMember => 5,
            ProductChildRole::IntersectionMember => 6,
            ProductChildRole::ProductMember => 7,
        }
    }
}

impl TryFrom<u8> for ProductChildRole {
    type Error = ProductChildRoleCodeError;

    /// Decodes a stable role discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u8) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::FunctionParameter),
            1 => Ok(Self::FunctionResult),
            2 => Ok(Self::GenericArgument),
            3 => Ok(Self::TupleElement),
            4 => Ok(Self::ArrayElement),
            5 => Ok(Self::UnionMember),
            6 => Ok(Self::IntersectionMember),
            7 => Ok(Self::ProductMember),
            actual => Err(ProductChildRoleCodeError { actual }),
        }
    }
}

/// Exact structural-constructor rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductConstructorFault {
    /// The observed tag is outside the closed constructor registry.
    Tag {
        /// Rejected tag value.
        actual: u32,
    },
    /// A payload cell is reserved for the tag and must be zero.
    ReservedPayload {
        /// Owning tag.
        tag: ProductConstructorTag,
        /// Rejected first payload cell.
        payload0: u32,
        /// Rejected second payload cell.
        payload1: u32,
    },
    /// The function parameter and result counts overflow the arity width.
    ArityOverflow {
        /// Owning tag.
        tag: ProductConstructorTag,
        /// Rejected first payload cell.
        payload0: u32,
        /// Rejected second payload cell.
        payload1: u32,
    },
    /// The validated child count differs from the constructor arity.
    Arity {
        /// Owning tag.
        tag: ProductConstructorTag,
        /// Required child count.
        expected: u32,
        /// Observed child count.
        actual: u32,
    },
}
