//! A stale C# oracle (`oracle.dll`) must be detected, not silently believed.
//!
//! # The exposure
//!
//! `tests/go/oracle_staleness.rs` documents a real incident: `lindsey.app`
//! ships no `nudox-go-oracle`, a user pointed `NUDOX_GO_ORACLE_BIN` at a
//! prebuilt binary that predated `references`, and every `refs` call for
//! every Go symbol answered `not_recorded` while the *source* produced
//! references correctly. The Go oracle got a version handshake for exactly
//! that reason.
//!
//! `NUDOX_CSHARP_ORACLE` is the identical door: `lindsey.app` does not ship
//! the Roslyn oracle either, so a user points that variable at whatever
//! `oracle.dll` they find. `schema::Extraction`'s fields are
//! `#[serde(default)]` for the same forward-compatibility reason the Go
//! payload's are, which makes an *older* `oracle.dll` omitting a field
//! (`references`, `diagnostics.generatorSupport`, `location`'s line/column
//! offsets) indistinguishable from a package that genuinely has none of it.
//! This guards the handshake that tells the two apart, before a C# field
//! report is what surfaces the gap.
//!
//! # Why a graceful parse, not a hard deserialize error
//!
//! `format` was already a required (non-`#[serde(default)]`) field before
//! this guard. That caught total absence, but as a raw `serde_json::Error`
//! with no diagnosis attached — the reader would see "missing field
//! `format`" and have to already know what that meant. Making the field
//! `#[serde(default)]` lets an old document still parse; [`staleness`] is
//! then asked separately, so the answer can be "your oracle is old" rather
//! than a bare deserialization failure.
//!
//! [`staleness`]: nudox_languages::csharp::schema::Extraction::staleness

use nudox_languages::csharp::schema::Extraction;

/// An oracle payload with no `format` at all — what an `oracle.dll` built
/// before this handshake existed emits. `assembly` is the one field on
/// [`Extraction`] with no `#[serde(default)]` of its own (every field inside
/// [`nudox_languages::csharp::schema::Assembly`] is defaulted), so it is the
/// only other key this minimal document needs.
const UNVERSIONED: &str = r#"{
  "assembly": {}
}"#;

/// The same payload, declaring the version this build reads.
fn versioned() -> String {
    UNVERSIONED.replacen(
        '{',
        &format!("{{\n  \"format\": {},", Extraction::REQUIRED_SCHEMA_VERSION),
        1,
    )
}

/// An oracle that declares no `format` at all must be reported as stale.
#[test]
fn an_unversioned_oracle_payload_is_reported_as_stale() {
    let extraction: Extraction = serde_json::from_str(UNVERSIONED).expect(
        "a document with no `format` must still DESERIALIZE — the point is to \
         diagnose it, not to make an old oracle unparseable",
    );

    let staleness = extraction
        .staleness()
        .expect("an oracle with no schema version is stale by definition");

    let rendered = staleness.to_string();
    assert!(
        rendered.contains("NUDOX_CSHARP_ORACLE") || rendered.contains("oracle.dll"),
        "the message must name the binary or the variable that points at it, \
         because rebuilding that binary is the entire fix: {rendered}",
    );
    assert!(
        rendered.contains("rebuild") || rendered.contains("out of date"),
        "the message must say the oracle is out of date rather than describing \
         a data problem — the whole point is not mistaking one for the other: \
         {rendered}",
    );
}

/// A current oracle is not reported as stale.
///
/// Guards the direction that would make the check worthless: a staleness
/// signal that fires always is one every reader learns to ignore.
#[test]
fn a_current_oracle_payload_is_not_stale() {
    let extraction: Extraction =
        serde_json::from_str(&versioned()).expect("a versioned payload must deserialize");

    assert!(
        extraction.staleness().is_none(),
        "an oracle declaring the required schema version must not be flagged",
    );
}

/// An oracle NEWER than this build is not stale.
///
/// The asymmetry is the point: `#[serde(default)]` exists so a newer oracle
/// can add fields an older reader ignores. Rejecting it would trade one false
/// report for another — exactly the failure mode the old exact-match `format`
/// check in `CSharpProducer::invoke` had, before this guard replaced it.
#[test]
fn a_newer_oracle_payload_is_not_stale() {
    let newer = UNVERSIONED.replacen(
        '{',
        &format!(
            "{{\n  \"format\": {},",
            Extraction::REQUIRED_SCHEMA_VERSION + 1
        ),
        1,
    );
    let extraction: Extraction =
        serde_json::from_str(&newer).expect("a newer payload must deserialize");

    assert!(
        extraction.staleness().is_none(),
        "a newer oracle is forward-compatible by construction; only an older \
         one under-reports",
    );
}

/// The committed C# oracle must stamp the format the Rust reader requires.
/// `lindsey.app` does not ship `oracle.dll`; a wrap that points
/// `NUDOX_CSHARP_ORACLE` at a prebuilt from this tree is how that stops
/// being a scavenger hunt, and the two numbers have to agree first.
#[test]
fn the_committed_csharp_oracle_source_declares_the_required_schema() {
    let src = include_str!("../../oracle/csharp/Program.cs");
    let needle = "SchemaFormat = ";
    let idx = src
        .find(needle)
        .expect("Program.cs must declare `SchemaFormat = N`");
    let declared: u32 = src[idx + needle.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("SchemaFormat must be an integer literal");
    assert_eq!(
        declared,
        Extraction::REQUIRED_SCHEMA_VERSION,
        "bump SchemaFormat in oracle/csharp/Program.cs in lockstep with \
         Extraction::REQUIRED_SCHEMA_VERSION",
    );
}
