//! Exhaustive boundary checks for fixed UTF-8 application text.

use wave_application_core::{INPUT_TEXT_BYTES, InputText, InputTextError, InputTextJoinError};

#[test]
fn segmented_input_preserves_every_utf8_boundary_and_exact_capacity() {
    let source = "aé🦀z";
    for first in 0..=source.len() {
        if !source.is_char_boundary(first) {
            continue;
        }
        for second in first..=source.len() {
            if !source.is_char_boundary(second) {
                continue;
            }
            let joined = InputText::try_from_parts(
                &source[..first],
                &source[first..second],
                &source[second..],
            );
            assert_eq!(joined.as_deref(), Ok(source));
        }
    }

    let accepted = "x".repeat(INPUT_TEXT_BYTES);
    assert_eq!(
        InputText::try_from_parts(&accepted[..1], &accepted[1..], "").as_deref(),
        Ok(accepted.as_str())
    );

    let rejected = "x".repeat(INPUT_TEXT_BYTES + 1);
    assert_eq!(
        InputText::try_from_parts(&rejected[..1], &rejected[1..], ""),
        Err(InputTextJoinError::InputTooLong(InputTextError {
            actual: INPUT_TEXT_BYTES + 1,
            maximum: INPUT_TEXT_BYTES,
        }))
    );
}
