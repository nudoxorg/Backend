use alloc::vec::Vec;

use crate::ir::{
    DeclarationIdentity, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
};

use super::catalog::{TypesReferenceV2, TypesRowDomainV2};
use super::{
    ATOM_LIST_TAG, ATOM_TAG, EXTERNAL_TARGET_TAG, FREE_PREDICATES_TAG, OBJECT_MEMBERS_TAG,
    ROOT_TAG, TEMPLATE_PARTS_TAG, TUPLE_ELEMENTS_TAG, TYPE_LIST_TAG, TYPE_PARAMETER_BOUNDS_TAG,
    TYPE_PARAMETERS_TAG, TYPE_TAG, TYPES_PLANE_CODE, TYPES_ROW_KEY_DOMAIN,
};

pub(super) fn typed_row_key(tag: u8, payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPES_ROW_KEY_DOMAIN);
    hasher.update(&[TYPES_PLANE_CODE, tag]);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}
pub(super) fn atom_key(bytes: &[u8]) -> Result<[u8; 32], SemanticPlaneRecordError> {
    let length = u32::try_from(bytes.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPES_ROW_KEY_DOMAIN);
    hasher.update(&[TYPES_PLANE_CODE, ATOM_TAG]);
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    Ok(*hasher.finalize().as_bytes())
}
pub(super) fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(identity.family.as_bytes());
    bytes[16..].copy_from_slice(identity.variant.as_bytes());
    bytes
}
pub(super) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SemanticPlaneRecordError> {
    out.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
            .to_be_bytes(),
    );
    out.extend_from_slice(bytes);
    Ok(())
}
/// Strict row dispatcher used by the common SPIR decoder.
pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if kind != SemanticPlaneKind::Ir(SemanticIrPlane::Types) {
        return Err(SemanticPlaneRecordError::PlaneKind);
    }
    let _ = parse_types_row(key, tag, payload)?;
    Ok(())
}

pub(super) struct ParsedTypesRow {
    pub(super) domain: TypesRowDomainV2,
    pub(super) root_identity: Option<[u8; 32]>,
    pub(super) root_type_present: Option<bool>,
    pub(super) references: Vec<TypesReferenceV2>,
    pub(super) declaration_references: Vec<[u8; 32]>,
}

pub(super) fn parse_types_row(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedTypesRow, SemanticPlaneRecordError> {
    match tag {
        ROOT_TAG => {
            if payload.len() != 65 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let identity: [u8; 32] = payload[..32]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
            let present = payload[32];
            let target: [u8; 32] = payload[33..65]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
            if present > 1 || (present == 0 && target != [0; 32]) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            if root_key_from_bytes(identity) != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let mut references = Vec::new();
            if present == 1 {
                references.push(TypesReferenceV2 {
                    domain: TypesRowDomainV2::Type,
                    key: target,
                });
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::EntityRoot,
                root_identity: Some(identity),
                root_type_present: Some(present == 1),
                references,
                declaration_references: Vec::new(),
            })
        }
        TYPE_TAG..=FREE_PREDICATES_TAG => parse_typed_node(key, tag, payload),
        ATOM_TAG => {
            let mut cursor = RowCursor::new(payload);
            let bytes = cursor.bytes32()?;
            cursor.finish()?;
            if atom_key(bytes)? != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::Atom,
                root_identity: None,
                root_type_present: None,
                references: Vec::new(),
                declaration_references: Vec::new(),
            })
        }
        EXTERNAL_TARGET_TAG => {
            let (identity, references) = external_identity_from_payload(payload)?;
            if identity != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::ExternalTarget,
                root_identity: None,
                root_type_present: None,
                references,
                declaration_references: Vec::new(),
            })
        }
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn parse_typed_node(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedTypesRow, SemanticPlaneRecordError> {
    let mut cursor = RowCursor::new(payload);
    let count =
        usize::try_from(cursor.u32()?).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let expected = count
        .checked_mul(38)
        .and_then(|length| length.checked_add(4))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    if payload.len() != expected {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(count)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    let mut previous_role = None;
    for _ in 0..count {
        let role = cursor.u8()?;
        let index = cursor.u32()?;
        let target_tag = cursor.u8()?;
        let target: [u8; 32] = cursor
            .take(32)?
            .try_into()
            .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
        if role > 25 || target_tag > 4 {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        if let Some((prior_index, prior_role)) = previous_role {
            if (index, role) < (prior_index, prior_role) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        previous_role = Some((index, role));
        edges.push(TypedWireEdge {
            role,
            index,
            target_tag,
            target,
        });
    }
    cursor.finish()?;

    let mut references = Vec::new();
    let mut declaration_references = Vec::new();
    let mut edges = TypedWireCursor::new(&edges);
    if tag == TYPE_TAG {
        let type_kind = edges.scalar(0, 0, |value| matches!(value, 1..=30 | 40..=49 | 60))?;
        validate_type_fields(
            type_kind,
            &mut edges,
            &mut references,
            &mut declaration_references,
        )?;
    } else {
        validate_list_node(
            tag,
            &mut edges,
            &mut references,
            &mut declaration_references,
        )?;
    }
    edges.finish()?;
    if typed_row_key(tag, payload) != key {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(ParsedTypesRow {
        domain: domain_for_tag(tag)?,
        root_identity: None,
        root_type_present: None,
        references,
        declaration_references,
    })
}

#[derive(Clone, Copy)]
struct TypedWireEdge {
    role: u8,
    index: u32,
    target_tag: u8,
    target: [u8; 32],
}

struct TypedWireCursor<'edges> {
    edges: &'edges [TypedWireEdge],
    position: usize,
}

impl<'edges> TypedWireCursor<'edges> {
    const fn new(edges: &'edges [TypedWireEdge]) -> Self {
        Self { edges, position: 0 }
    }

    fn take(
        &mut self,
        role: u8,
        index: u32,
        target_tag: u8,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        let edge = self
            .edges
            .get(self.position)
            .ok_or(SemanticPlaneRecordError::RowGrammar)?;
        if edge.role != role || edge.index != index || edge.target_tag != target_tag {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        self.position += 1;
        Ok(edge.target)
    }

    fn scalar(
        &mut self,
        role: u8,
        index: u32,
        valid: impl FnOnce(u64) -> bool,
    ) -> Result<u64, SemanticPlaneRecordError> {
        let bytes = self.take(role, index, 4)?;
        if bytes[..24] != [0; 24] {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        let value = u64::from_be_bytes(
            bytes[24..]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?,
        );
        if valid(value) {
            Ok(value)
        } else {
            Err(SemanticPlaneRecordError::RowGrammar)
        }
    }

    fn node(
        &mut self,
        role: u8,
        index: u32,
        domain: TypesRowDomainV2,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let key = self.take(role, index, 0)?;
        references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        references.push(TypesReferenceV2 { domain, key });
        Ok(())
    }

    fn atom(
        &mut self,
        role: u8,
        index: u32,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.reference(role, index, 1, TypesRowDomainV2::Atom, references)
    }

    fn external(
        &mut self,
        role: u8,
        index: u32,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.reference(role, index, 3, TypesRowDomainV2::ExternalTarget, references)
    }

    fn reference(
        &mut self,
        role: u8,
        index: u32,
        target_tag: u8,
        domain: TypesRowDomainV2,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let key = self.take(role, index, target_tag)?;
        references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        references.push(TypesReferenceV2 { domain, key });
        Ok(())
    }

    fn entity(
        &mut self,
        role: u8,
        index: u32,
        declaration_references: &mut Vec<[u8; 32]>,
    ) -> Result<(), SemanticPlaneRecordError> {
        declaration_references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        declaration_references.push(self.take(role, index, 2)?);
        Ok(())
    }

    fn peek_index(&self) -> Option<u32> {
        self.edges.get(self.position).map(|edge| edge.index)
    }

    const fn is_empty(&self) -> bool {
        self.position == self.edges.len()
    }

    fn finish(self) -> Result<(), SemanticPlaneRecordError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::RowGrammar)
        }
    }
}

fn validate_type_fields(
    kind: u64,
    edges: &mut TypedWireCursor<'_>,
    references: &mut Vec<TypesReferenceV2>,
    declaration_references: &mut Vec<[u8; 32]>,
) -> Result<(), SemanticPlaneRecordError> {
    macro_rules! scalar {
        ($field:expr, $valid:expr) => {
            edges.scalar(1, $field, $valid)?
        };
    }
    macro_rules! node {
        ($field:expr, $domain:expr) => {
            edges.node(1, $field, $domain, references)?
        };
    }
    macro_rules! atom {
        ($field:expr) => {
            edges.atom(1, $field, references)?
        };
    }
    macro_rules! external {
        ($field:expr) => {
            edges.external(1, $field, references)?
        };
    }
    macro_rules! entity {
        ($field:expr) => {
            edges.entity(1, $field, declaration_references)?
        };
    }
    const BOOL: fn(u64) -> bool = |value| value <= 1;
    const ANY: fn(u64) -> bool = |_| true;
    const MUTABILITY: fn(u64) -> bool = |value| value <= 1;
    const VARIADIC: fn(u64) -> bool = |value| value <= 2;
    const MODIFIER: fn(u64) -> bool = |value| value <= 2;

    use TypesRowDomainV2::{AtomList, ObjectMembers, TemplateParts, TupleElements, Type, TypeList};
    match kind {
        1 => {
            if !matches!(
                scalar!(0, ANY),
                0..=28 | 30..=34 | 36..=41
            ) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        2 => match scalar!(0, |value| value <= 5) {
            0..=2 => atom!(1),
            3 => {
                scalar!(1, BOOL);
            }
            4 | 5 => {}
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        3 => entity!(0),
        4 => external!(0),
        5 => atom!(0),
        6 => {
            node!(0, Type);
            node!(1, TypeList);
        }
        7 => node!(0, TupleElements),
        8 => node!(0, ObjectMembers),
        9 => {
            node!(0, TupleElements);
            node!(1, TupleElements);
            if scalar!(2, BOOL) == 1 {
                atom!(3);
            }
            scalar!(4, VARIADIC);
            scalar!(5, BOOL);
        }
        10 => {
            node!(0, Type);
            scalar!(1, MUTABILITY);
            if scalar!(2, BOOL) == 1 {
                atom!(3);
            }
        }
        11 => {
            node!(0, Type);
            scalar!(1, |value| value <= 1);
        }
        12 => node!(0, Type),
        13 => {
            node!(0, Type);
            node!(1, Type);
        }
        14 => {
            node!(0, Type);
            scalar!(1, |value| value <= u8::MAX as u64);
        }
        15 => node!(0, Type),
        16 => {
            scalar!(0, |value| value <= 9);
            scalar!(1, |value| (1..=u16::MAX as u64).contains(&value));
        }
        17 => {
            node!(0, Type);
            scalar!(1, MUTABILITY);
        }
        18 => node!(0, Type),
        19 => {
            node!(0, Type);
            match scalar!(1, |value| value <= 4) {
                0 | 4 => {}
                1 => {
                    scalar!(2, |value| (1..=u16::MAX as u64).contains(&value));
                }
                2 => {
                    scalar!(2, ANY);
                }
                3 => atom!(2),
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        20 => node!(0, Type),
        21..=24 => node!(0, TypeList),
        25 => match scalar!(0, |value| value <= 2) {
            0 => {}
            1 | 2 => node!(1, Type),
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        26 => {
            scalar!(0, |value| value <= 3);
            node!(1, Type);
        }
        27 => {
            if scalar!(0, BOOL) == 1 {
                atom!(1);
            }
        }
        28 => {
            node!(0, Type);
            if scalar!(1, BOOL) == 1 {
                node!(2, Type);
            }
            if scalar!(3, BOOL) == 1 {
                node!(4, AtomList);
            }
            atom!(5);
        }
        29 => {
            node!(0, Type);
            node!(1, Type);
        }
        30 => {
            scalar!(0, |value| value <= 2);
            node!(1, Type);
        }
        40 => node!(0, Type),
        41 => match scalar!(0, |value| value <= 2) {
            0 => entity!(1),
            1 => node!(1, AtomList),
            2 => external!(1),
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        42 => {
            node!(0, Type);
            node!(1, Type);
        }
        43 => {
            node!(0, Type);
            node!(1, Type);
            node!(2, Type);
            node!(3, Type);
            scalar!(4, BOOL);
        }
        44 => {
            atom!(0);
            node!(1, Type);
            if scalar!(2, BOOL) == 1 {
                node!(3, Type);
            }
            node!(4, Type);
            scalar!(5, MODIFIER);
            scalar!(6, MODIFIER);
        }
        45 => {
            atom!(0);
            if scalar!(1, BOOL) == 1 {
                node!(2, Type);
            }
        }
        46 => node!(0, TemplateParts),
        47 => {
            atom!(0);
            node!(1, AtomList);
            node!(2, TypeList);
        }
        48 => node!(0, Type),
        49 => {}
        60 => {
            scalar!(0, |value| value <= 7);
            if scalar!(1, BOOL) == 1 {
                atom!(2);
            }
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    Ok(())
}

fn validate_list_node(
    tag: u8,
    edges: &mut TypedWireCursor<'_>,
    references: &mut Vec<TypesReferenceV2>,
    _declaration_references: &mut Vec<[u8; 32]>,
) -> Result<(), SemanticPlaneRecordError> {
    use TypesRowDomainV2::{Type, TypeParameterBounds};
    let mut expected_index = 0_u32;
    while !edges.is_empty() {
        if edges.peek_index() != Some(expected_index) {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        match tag {
            TYPE_LIST_TAG => edges.node(2, expected_index, Type, references)?,
            ATOM_LIST_TAG => edges.atom(2, expected_index, references)?,
            TUPLE_ELEMENTS_TAG => {
                if edges.scalar(3, expected_index, |value| value <= 1)? == 1 {
                    edges.atom(3, expected_index, references)?;
                }
                edges.node(4, expected_index, Type, references)?;
                edges.scalar(5, expected_index, |value| value <= 2)?;
            }
            OBJECT_MEMBERS_TAG => match edges.scalar(6, expected_index, |value| value <= 4)? {
                0 => {
                    validate_object_key(edges, expected_index, references)?;
                    edges.node(8, expected_index, Type, references)?;
                    edges.scalar(9, expected_index, |value| value <= 1)?;
                    edges.scalar(10, expected_index, |value| value <= 1)?;
                }
                1 => {
                    validate_object_key(edges, expected_index, references)?;
                    edges.node(8, expected_index, Type, references)?;
                    edges.scalar(9, expected_index, |value| value <= 1)?;
                }
                2 => {
                    edges.node(7, expected_index, Type, references)?;
                    edges.scalar(10, expected_index, |value| value <= 1)?;
                    edges.atom(11, expected_index, references)?;
                    edges.node(12, expected_index, Type, references)?;
                }
                3 | 4 => edges.node(8, expected_index, Type, references)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            },
            TEMPLATE_PARTS_TAG => match edges.scalar(13, expected_index, |value| value <= 1)? {
                0 => edges.atom(13, expected_index, references)?,
                1 => edges.node(13, expected_index, Type, references)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            },
            TYPE_PARAMETERS_TAG => {
                edges.atom(14, expected_index, references)?;
                edges.node(15, expected_index, TypeParameterBounds, references)?;
                if edges.scalar(16, expected_index, |value| value <= 1)? == 1 {
                    edges.node(16, expected_index, Type, references)?;
                }
                edges.scalar(17, expected_index, |value| value <= 3)?;
                match edges.scalar(18, expected_index, |value| value <= 2)? {
                    0 => {
                        edges.scalar(18, expected_index, |value| value <= 1)?;
                    }
                    1 => edges.node(18, expected_index, Type, references)?,
                    2 => {}
                    _ => return Err(SemanticPlaneRecordError::RowGrammar),
                }
                edges.scalar(19, expected_index, |value| value <= 6)?;
                edges.scalar(20, expected_index, |value| value <= 1)?;
                edges.scalar(21, expected_index, |value| value <= 1)?;
            }
            TYPE_PARAMETER_BOUNDS_TAG => {
                match edges.scalar(22, expected_index, |value| value <= 1)? {
                    0 => edges.node(23, expected_index, Type, references)?,
                    1 => edges.atom(23, expected_index, references)?,
                    _ => return Err(SemanticPlaneRecordError::RowGrammar),
                }
            }
            FREE_PREDICATES_TAG => {
                edges.node(24, expected_index, Type, references)?;
                edges.node(25, expected_index, TypeParameterBounds, references)?;
            }
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        }
        expected_index = expected_index
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    }
    Ok(())
}

fn validate_object_key(
    edges: &mut TypedWireCursor<'_>,
    index: u32,
    references: &mut Vec<TypesReferenceV2>,
) -> Result<(), SemanticPlaneRecordError> {
    match edges.scalar(7, index, |value| value <= 3)? {
        0..=2 => edges.atom(7, index, references),
        3 => edges.node(7, index, TypesRowDomainV2::Type, references),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn domain_for_tag(tag: u8) -> Result<TypesRowDomainV2, SemanticPlaneRecordError> {
    Ok(match tag {
        TYPE_TAG => TypesRowDomainV2::Type,
        TYPE_LIST_TAG => TypesRowDomainV2::TypeList,
        TUPLE_ELEMENTS_TAG => TypesRowDomainV2::TupleElements,
        OBJECT_MEMBERS_TAG => TypesRowDomainV2::ObjectMembers,
        TEMPLATE_PARTS_TAG => TypesRowDomainV2::TemplateParts,
        ATOM_LIST_TAG => TypesRowDomainV2::AtomList,
        TYPE_PARAMETERS_TAG => TypesRowDomainV2::TypeParameters,
        TYPE_PARAMETER_BOUNDS_TAG => TypesRowDomainV2::TypeParameterBounds,
        FREE_PREDICATES_TAG => TypesRowDomainV2::FreePredicates,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}

impl TypesRowDomainV2 {
    pub(super) const fn is_typed_node(self) -> bool {
        matches!(
            self,
            Self::Type
                | Self::TypeList
                | Self::TupleElements
                | Self::ObjectMembers
                | Self::TemplateParts
                | Self::AtomList
                | Self::TypeParameters
                | Self::TypeParameterBounds
                | Self::FreePredicates
        )
    }
}

fn root_key_from_bytes(identity: [u8; 32]) -> [u8; 32] {
    identity
}

struct RowCursor<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}
impl<'bytes> RowCursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, length: usize) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SemanticPlaneRecordError::Truncated)?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, SemanticPlaneRecordError> {
        Ok(*self
            .take(1)?
            .first()
            .ok_or(SemanticPlaneRecordError::Truncated)?)
    }
    fn u16(&mut self) -> Result<u16, SemanticPlaneRecordError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, SemanticPlaneRecordError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }
    fn bytes32(&mut self) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        self.take(length)
    }
    fn finish(self) -> Result<(), SemanticPlaneRecordError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::RowTrailingBytes)
        }
    }
}

pub(super) fn external_identity_from_payload(
    payload: &[u8],
) -> Result<([u8; 32], Vec<TypesReferenceV2>), SemanticPlaneRecordError> {
    let mut cursor = RowCursor::new(payload);
    let mut hasher = blake3::Hasher::new();
    let mut references = Vec::new();
    hasher.update(b"compiler-ir.external-target.v1\0");
    match cursor.u8()? {
        0 => {
            hasher.update(&[0]);
            hasher.update(cursor.take(32)?);
            hasher.update(cursor.take(32)?);
        }
        1 => {
            hasher.update(&[1]);
            hasher.update(cursor.take(16)?);
            match cursor.u8()? {
                0 => hasher.update(&[0]),
                1 => {
                    hasher.update(&[1]);
                    hasher.update(cursor.take(16)?);
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            match cursor.u8()? {
                0 => {
                    hasher.update(&[0]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                1 => {
                    hasher.update(&[1]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                2 => {
                    hasher.update(&[2]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                3 => {
                    hasher.update(&[3]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
            match cursor.u8()? {
                0 => hasher.update(&[0]),
                1 => {
                    hasher.update(&[1]);
                    let kind = cursor.u16()?;
                    if crate::ir::ItemKind::try_from(kind).is_err() {
                        return Err(SemanticPlaneRecordError::RowGrammar);
                    }
                    hasher.update(&kind.to_be_bytes());
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        2 => {
            hasher.update(&[2]);
            hasher.update(cursor.take(32)?);
            hasher.update(cursor.take(4)?);
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    cursor.finish()?;
    Ok((*hasher.finalize().as_bytes(), references))
}

fn hash_external_atom_cell(
    cursor: &mut RowCursor<'_>,
    hasher: &mut blake3::Hasher,
    references: &mut Vec<TypesReferenceV2>,
) -> Result<(), SemanticPlaneRecordError> {
    let bytes = cursor.bytes32()?;
    let length = u64::try_from(bytes.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    references
        .try_reserve(1)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    references.push(TypesReferenceV2 {
        domain: TypesRowDomainV2::Atom,
        key: atom_key(bytes)?,
    });
    Ok(())
}
