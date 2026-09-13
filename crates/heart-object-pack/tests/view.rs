//! Exercises the `heart-object-pack` tests view contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public complete-pack lookup, borrowing, and selected identity contracts.

use core::mem::{align_of, size_of};
use std::vec::Vec;

use allocation_counter::{AllocationInfo, measure};
use heart_identity::{ContentId, ObjectDomain};
use heart_object::{ObjectLength, ObjectRef};
use heart_object_pack::{
    OBJECT_PACK_HEADER_BYTES, ObjectPackBytes, ObjectPackError, ObjectPackHeader, ObjectPackIndex,
    ObjectPackView, PackInput, PreparedObjectPack,
};
use heart_schema::SchemaId;
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error(transparent)]
    Length(#[from] core::num::TryFromIntError),
    #[error("fixture lookup unexpectedly missed")]
    Missing,
    #[error("unexpected object was present for an absent identity")]
    UnexpectedPresent,
    #[error("invalid complete pack unexpectedly opened")]
    UnexpectedView,
    #[error("allocation measurement did not execute")]
    MeasurementDidNotRun,
}

type Fixture = (Vec<u8>, [PackInput<'static>; 3]);

fn input(bytes: &'static [u8]) -> Result<PackInput<'static>, TestError> {
    Ok(PackInput {
        reference: ObjectRef {
            content: ContentId::from_canonical_bytes(bytes),
            length: ObjectLength::from(u64::try_from(bytes.len())?),
            schema: SchemaId::Object,
            kind: 7_u16.into(),
        },
        bytes,
    })
}

fn pack() -> Result<Fixture, TestError> {
    let mut inputs = [input(b"a")?, input(b"middle")?, input(b"tail")?];
    inputs.sort_by_key(|input| input.reference.content);
    let prepared = PreparedObjectPack::prepare(&inputs)?;
    let mut bytes = vec![0; usize::from(prepared.required_bytes)];
    prepared.write(&mut bytes)?;
    Ok((bytes, inputs))
}

fn index_extent(bytes: &[u8]) -> Result<usize, TestError> {
    let header = bytes
        .get(..OBJECT_PACK_HEADER_BYTES)
        .ok_or(TestError::Missing)?;
    Ok(ObjectPackHeader::try_from(header)?.index_bytes.into())
}

fn object<'pack>(
    view: &'pack ObjectPackView<'pack>,
    content: ContentId<ObjectDomain>,
) -> Result<heart_object_pack::ObjectPackObject<'pack>, TestError> {
    view.lookup(&content).ok_or(TestError::Missing)
}

fn verified_body<'pack>(
    view: &ObjectPackView<'pack>,
    content: ContentId<ObjectDomain>,
) -> Result<&'pack [u8], TestError> {
    let selected = view.lookup(&content).ok_or(TestError::Missing)?;
    Ok(selected.verify()?)
}

fn opening_error(bytes: &[u8]) -> Result<ObjectPackError, TestError> {
    match ObjectPackView::try_from(bytes) {
        Err(error) => Ok(error),
        Ok(_) => Err(TestError::UnexpectedView),
    }
}

fn absent(view: &ObjectPackView<'_>, content: ContentId<ObjectDomain>) -> Result<(), TestError> {
    match view.lookup(&content) {
        None => Ok(()),
        Some(_) => Err(TestError::UnexpectedPresent),
    }
}

#[test]
fn complete_view_binary_search_lends_exact_bodies_without_neighbor_bleed() -> Result<(), TestError>
{
    let empty_bytes = [0; OBJECT_PACK_HEADER_BYTES];
    let empty = ObjectPackView::try_from(empty_bytes.as_slice())?;
    absent(&empty, ContentId::from_digest([0; 32]))?;
    let only = input(b"only")?;
    let one_input = [only];
    let prepared = PreparedObjectPack::prepare(&one_input)?;
    let mut one_bytes = vec![0; usize::from(prepared.required_bytes)];
    prepared.write(&mut one_bytes)?;
    let one = ObjectPackView::try_from(one_bytes.as_slice())?;
    assert_eq!(object(&one, only.reference.content)?.body, only.bytes);
    let (bytes, inputs) = pack()?;
    let view = ObjectPackView::try_from(bytes.as_slice())?;
    let body_start = index_extent(bytes.as_slice())?;
    let mut offset = body_start;
    for input in inputs {
        let selected = object(&view, input.reference.content)?;
        assert_eq!(selected.reference, input.reference);
        assert_eq!(selected.body, input.bytes);
        assert_eq!(selected.body.as_ptr(), bytes.as_ptr().wrapping_add(offset));
        assert_eq!(selected.verify()?, input.bytes);
        offset += input.bytes.len();
    }
    let first = inputs[0].reference.content;
    let last = inputs[2].reference.content;
    let mut below = *first;
    let mut above = *last;
    let below_byte = below
        .iter_mut()
        .rev()
        .find(|byte| **byte != 0)
        .ok_or(TestError::Missing)?;
    *below_byte -= 1;
    let above_byte = above
        .iter_mut()
        .rev()
        .find(|byte| **byte != u8::MAX)
        .ok_or(TestError::Missing)?;
    *above_byte += 1;
    absent(&view, ContentId::from_digest(below))?;
    absent(&view, ContentId::from_digest(above))?;
    absent(&view, ContentId::from_digest([0x55; 32]))?;
    Ok(())
}

#[test]
fn complete_view_reports_exact_extents_and_selected_hash_mismatch() -> Result<(), TestError> {
    let (bytes, inputs) = pack()?;
    let index_bytes = index_extent(bytes.as_slice())?;
    let expected = ObjectPackBytes::from(bytes.len());
    assert_eq!(
        opening_error(
            bytes
                .get(..OBJECT_PACK_HEADER_BYTES - 1)
                .ok_or(TestError::Missing)?
        )?,
        ObjectPackError::HeaderTruncated {
            required: OBJECT_PACK_HEADER_BYTES.into(),
            available: (OBJECT_PACK_HEADER_BYTES - 1).into()
        }
    );
    assert_eq!(
        opening_error(bytes.get(..index_bytes - 1).ok_or(TestError::Missing)?)?,
        ObjectPackError::DirectoryExtent {
            expected: index_bytes.into(),
            actual: (index_bytes - 1).into()
        }
    );
    assert_eq!(
        opening_error(bytes.get(..index_bytes).ok_or(TestError::Missing)?)?,
        ObjectPackError::PackExtent {
            expected,
            actual: index_bytes.into()
        }
    );
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        opening_error(trailing.as_slice())?,
        ObjectPackError::PackExtent {
            expected,
            actual: (bytes.len() + 1).into()
        }
    );
    let mut wrong = bytes.clone();
    let first_body = index_bytes;
    *wrong.get_mut(first_body).ok_or(TestError::Missing)? ^= 1;
    let wrong_view = ObjectPackView::try_from(wrong.as_slice())?;
    let expected_id = inputs[0].reference.content;
    let actual = ContentId::from_canonical_bytes(object(&wrong_view, expected_id)?.body);
    assert_eq!(
        object(&wrong_view, expected_id)?.verify(),
        Err(ObjectPackError::ObjectContent {
            expected: expected_id,
            actual
        })
    );
    Ok(())
}

#[test]
fn view_and_index_are_compact_and_lookup_verify_allocate_nothing() -> Result<(), TestError> {
    let (bytes, inputs) = pack()?;
    let view = ObjectPackView::try_from(bytes.as_slice())?;
    let expected = inputs[1].reference.content;
    let warm = object(&view, expected)?;
    assert_eq!(warm.verify()?, inputs[1].bytes);
    let verified = verified_body(&view, expected)?;
    assert_eq!(verified, inputs[1].bytes);
    assert_eq!(verified.as_ptr(), warm.body.as_ptr());
    let mut result = None;
    let allocations = measure(|| {
        result = Some(
            view.lookup(&expected)
                .ok_or(TestError::Missing)
                .and_then(|selected| {
                    selected.verify()?;
                    Ok(selected.body)
                }),
        );
    });
    let body = result.ok_or(TestError::MeasurementDidNotRun)??;
    assert_eq!(body, inputs[1].bytes);
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0
        }
    );
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<ObjectPackIndex<'_>>(), 56);
        assert_eq!(align_of::<ObjectPackIndex<'_>>(), 8);
        assert_eq!(size_of::<ObjectPackView<'_>>(), 32);
        assert_eq!(align_of::<ObjectPackView<'_>>(), 8);
    }
    #[cfg(target_pointer_width = "32")]
    {
        assert_eq!(size_of::<ObjectPackIndex<'_>>(), 32);
        assert_eq!(align_of::<ObjectPackIndex<'_>>(), 4);
        assert_eq!(size_of::<ObjectPackView<'_>>(), 16);
        assert_eq!(align_of::<ObjectPackView<'_>>(), 4);
    }
    Ok(())
}
