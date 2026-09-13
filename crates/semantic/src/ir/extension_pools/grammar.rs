//! Private discriminants shared by the current encoder and every compatible
//! decoder. Keeping this vocabulary in one place prevents a schema writer and
//! reader from silently assigning different meanings to the same byte.

pub(super) const PRESENCE_NONE: u8 = 0;
pub(super) const PRESENCE_SOME: u8 = 1;

pub(super) const TYPE_PARAMETER_BOUND_TYPE: u8 = 0;
pub(super) const TYPE_PARAMETER_BOUND_LIFETIME: u8 = 1;

pub(super) const TYPE_PARAMETER_KIND_TYPE: u8 = 0;
pub(super) const TYPE_PARAMETER_KIND_CONST_INFERENCE: u8 = 1;
pub(super) const TYPE_PARAMETER_KIND_CONST_VALUE: u8 = 2;
pub(super) const TYPE_PARAMETER_KIND_LIFETIME: u8 = 3;

pub(super) const VARIANCE_INVARIANT: u8 = 0;
pub(super) const VARIANCE_COVARIANT: u8 = 1;
pub(super) const VARIANCE_CONTRAVARIANT: u8 = 2;
pub(super) const VARIANCE_BIVARIANT: u8 = 3;

pub(super) const PRIMARY_REQUIREMENT_NONE: u8 = 0;
pub(super) const PRIMARY_REQUIREMENT_REFERENCE: u8 = 1;
pub(super) const PRIMARY_REQUIREMENT_NULLABLE_REFERENCE: u8 = 2;
pub(super) const PRIMARY_REQUIREMENT_VALUE: u8 = 3;
pub(super) const PRIMARY_REQUIREMENT_UNMANAGED: u8 = 4;
pub(super) const PRIMARY_REQUIREMENT_NOT_NULL: u8 = 5;
pub(super) const PRIMARY_REQUIREMENT_DEFAULT: u8 = 6;

pub(super) const REQUIREMENT_CONSTRUCTOR: u8 = 1;
pub(super) const REQUIREMENT_ALLOWS_REF_LIKE: u8 = 2;
