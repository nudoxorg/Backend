//! A stale Java oracle doclet must be detected, not silently believed.
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
//! `NUDOX_JAVA_ORACLE_CLASSES` is the identical door, documented in
//! `src/java/invoke.rs`'s module doc as "a runtime escape hatch … for
//! pointing at a hand-built classes directory during doclet development" —
//! the same override shape as `NUDOX_GO_ORACLE_BIN`, deliberately. A classes
//! directory built from an older checkout is exactly as capable of silently
//! under-reporting `references` as an old Go binary is, for the identical
//! reason: `schema::Extraction`'s fields are `#[serde(default)]`, so an old
//! doclet's absent field and a package's genuinely empty one deserialize to
//! the same empty `Box<[_]>`.
//!
//! # No emitter change here — read `src/java/invoke.rs` first
//!
//! Unlike the Go oracle (a binary this repo builds and ships instructions
//! for) and the C# oracle (`oracle/csharp/Extractor.cs`, owned by this
//! crate), the Java doclet's `format` field
//! (`oracle/java/Extractor.java:182`, `json.value(1)`) is not something this
//! change touches or bumps: `build.rs` compiles it into `$OUT_DIR/classes` at
//! this crate's own build time, so the classes directory `JavaProducer` runs
//! by default is always built from the current source tree, and there is no
//! separately-published binary to fall behind the way `nudox-go-oracle` or
//! `oracle.dll` can. The only way a stale payload reaches this build is
//! through the `NUDOX_JAVA_ORACLE_CLASSES` override — a genuinely older
//! classes directory kept around by hand.
//!
//! This test therefore only exercises detection: [`Extraction::staleness`]
//! diagnoses a payload with no schema version as stale. There is no
//! oracle-side change to pair it with, because there is no oracle source this
//! crate ships that could emit a wrong version by hand.

use nudox_languages::java::schema::Extraction;

/// An oracle payload with no `format` at all — what a classes directory built
/// before this handshake existed produces. `javaVersion` is the one other
/// field on [`Extraction`] with no `#[serde(default)]`.
const UNVERSIONED: &str = r#"{
  "javaVersion": "21"
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
         diagnose it, not to make an old classes directory unparseable",
    );

    let staleness = extraction
        .staleness()
        .expect("an oracle with no schema version is stale by definition");

    let rendered = staleness.to_string();
    assert!(
        rendered.contains("NUDOX_JAVA_ORACLE_CLASSES"),
        "the message must name the variable that points at the stale classes \
         directory, because repointing (or rebuilding) it is the entire fix: \
         {rendered}",
    );
    assert!(
        rendered.contains("out of date") || rendered.contains("older"),
        "the message must say the doclet is out of date rather than describing \
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
/// report for another.
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

/// The committed doclet must stamp the format the Rust reader requires.
/// `build.rs` compiles this file into `$OUT_DIR/classes`; a wrap that points
/// `NUDOX_JAVA_ORACLE_CLASSES` at an older classes dir is the remaining
/// stale-oracle door, and this is what tells those two numbers apart.
#[test]
fn the_committed_java_oracle_source_declares_the_required_schema() {
    let src = include_str!("../../oracle/java/Extractor.java");
    let name = src
        .find("json.name(\"format\")")
        .expect("Extractor.run must stamp a format field");
    let after = &src[name..];
    let value: u32 = after
        .find("json.value(")
        .and_then(|i| {
            after[i + "json.value(".len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .expect("json.name(\"format\") must be followed by json.value(N)");
    assert_eq!(
        value,
        Extraction::REQUIRED_SCHEMA_VERSION,
        "bump json.value(N) in Extractor.java in lockstep with \
         Extraction::REQUIRED_SCHEMA_VERSION",
    );
}
