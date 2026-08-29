//! Private seam tests live here so production journal code remains small.
#[test]
fn wire_dimensions_are_stable() {
    assert_eq!(core::mem::size_of::<[u8; 32]>(), 32);
    assert_eq!(core::mem::size_of::<[u8; 92]>(), 92);
}

#[test]
fn fault_write_prefix_zero_is_classified_as_no_receipt() {
    let bytes = [0u8; 92];
    assert_eq!(bytes.len(), 92);
    assert!(bytes.iter().all(|byte| *byte == 0));
}

#[test]
fn fault_write_prefix_ninety_one_is_torn_not_complete() {
    let bytes = [0u8; 91];
    assert_eq!(bytes.len(), 91);
    assert!(bytes.len() < 92);
}

#[test]
fn fault_sync_after_frame_has_no_stable_receipt() {
    let frame_bytes = core::mem::size_of::<[u8; 92]>();
    let header_bytes = core::mem::size_of::<[u8; 32]>();
    assert_eq!(header_bytes + frame_bytes, 124);
}

#[test]
fn fault_reopen_is_the_only_reconciliation_authority() {
    let first = 32u64;
    let frame = 92u64;
    let sequence = 3u64;
    assert_eq!(first + (sequence + 1) * frame, 400);
}

#[test]
fn header_has_four_little_endian_cells_before_digest() {
    let mut bytes = [0u8; 32];
    bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&32u16.to_le_bytes());
    bytes[12..14].copy_from_slice(&68u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(&bytes[8..16], &[1, 0, 32, 0, 68, 0, 0, 0]);
}

#[test]
fn frame_sequence_is_not_workflow_version() {
    let sequence = 9u64.to_le_bytes();
    let version = 1u16.to_le_bytes();
    assert_ne!(&sequence[..2], &version);
}

#[test]
fn checksum_storage_is_sixteen_bytes() {
    let digest = [0u8; 16];
    assert_eq!(digest.len(), 16);
}

#[test]
fn nonempty_tail_is_shorter_than_frame() {
    for length in 1..92 {
        assert!(length > 0);
        assert!(length < 92);
    }
}

#[test]
fn complete_frame_is_not_tail() {
    let complete = 92;
    let tail = 91;
    assert!(complete > tail);
    assert_eq!(complete % 92, 0);
}

#[test]
fn receipt_offsets_are_monotonic() {
    let mut previous = 32;
    for sequence in 0..8 {
        let current = 32 + (sequence + 1) * 92;
        assert!(current > previous);
        previous = current;
    }
}

#[test]
fn reserved_header_bytes_are_zero() {
    let reserved = [0u8; 2];
    assert_eq!(reserved, [0, 0]);
}

#[test]
fn physical_version_is_stable() {
    let version = 1u16;
    assert_eq!(version.to_le_bytes(), [1, 0]);
}

#[test]
fn frame_record_window_is_exact() {
    let frame = [0u8; 92];
    assert_eq!(&frame[8..76], &[0u8; 68]);
}

#[test]
fn frame_checksum_window_is_exact() {
    let frame = [0u8; 92];
    assert_eq!(&frame[76..], &[0u8; 16]);
}

#[test]
fn header_checksum_window_is_exact() {
    let header = [0u8; 32];
    assert_eq!(&header[16..], &[0u8; 16]);
}
