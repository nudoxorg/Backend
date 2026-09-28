//! Oracle for untrusted `NDX1` store frames.
//!
//! The public entry is [`ValidatedFrame::validate`]. There is no public
//! encoder, so `canonical` returns the known one-section literal. The in-crate
//! mutation laws stay `pub(super)` and are not called from here.

use backend_store::view::{Section, SectionReadError, ValidatedFrame};
use backend_version::schema::MAX_FRAME_BYTES;

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
/// It is below [`MAX_FRAME_BYTES`] so a campaign is not spent on megabyte pads.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

/// Known one-section frame. The bytes match the private store fixture.
const CANONICAL: [u8; 41] = [
    b'N', b'D', b'X', b'1', 1, 0, 40, 0, 41, 0, 0, 0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 3, 0, 0,
    0, 40, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0xa5,
];

const _: () = assert!(CANONICAL.len() == 41);
const _: () = assert!(MAX_LEN >= CANONICAL.len());
const _: () = assert!(MAX_FRAME_BYTES == 1_048_576);
const _: () = assert!(MAX_LEN <= 1_048_576);

/// The committed `canonical` seed.
///
/// # Errors
///
/// This fixture is a literal, so construction does not fail. The discovered
/// target table still requires a `Result`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "Target::canonical is fn() -> Result<Vec<u8>, String>"
)]
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    Ok(CANONICAL.to_vec())
}

/// Replays validation and the section-reborrow law.
///
/// # Errors
///
/// Returns when two validations disagree, a validated section cannot be
/// reborrowed, or a section body does not lie inside the frame.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after the public frame validator runs.
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`].
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let first = ValidatedFrame::validate(bytes);
    let second = ValidatedFrame::validate(bytes);
    if first != second {
        return Err(OracleFailure::new("frame validation is not deterministic"));
    }
    let Ok(frame) = first else {
        return Ok(Verdict::Rejected);
    };
    if bytes.len() > 1_048_576 {
        return Err(OracleFailure::new(
            "validator accepted a frame over the protocol byte cap",
        ));
    }
    if &*frame != bytes {
        return Err(OracleFailure::new(
            "validated frame bytes differ from the input",
        ));
    }
    let left = read_sections(&frame)?;
    let right = read_sections(&frame)?;
    if left != right {
        return Err(OracleFailure::new("section reborrow is not deterministic"));
    }
    Ok(Verdict::Accepted)
}

fn read_sections<'a>(frame: &'a ValidatedFrame<'a>) -> Result<Vec<Section<'a>>, OracleFailure> {
    let mut sections = Vec::new();
    for item in frame.sections() {
        let section = item.map_err(|error: SectionReadError| {
            OracleFailure::new(format!(
                "validated section could not be reborrowed: {error}"
            ))
        })?;
        if !body_inside(frame, section.bytes) {
            return Err(OracleFailure::new(
                "section body is outside the validated frame",
            ));
        }
        sections.push(section);
    }
    Ok(sections)
}

fn body_inside(frame: &[u8], body: &[u8]) -> bool {
    let span = frame.as_ptr_range();
    let found = body.as_ptr_range();
    found.start >= span.start && found.end <= span.end
}
