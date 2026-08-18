//! A stale oracle binary must be detected, not silently believed.
//!
//! # The incident
//!
//! Field report, 2026-08-16, against a real Go service:
//!
//! > `refs` (both directions) — completely dead for this Go index, every
//! > single call came back `~refs:not_recorded(go)`. … `graph subtypes` — the
//! > documented Go equivalent, but came back empty too, so Go's implicit
//! > interface satisfaction isn't tracked at all in this index.
//!
//! Neither was true of the source. `lindsey.app` does not ship
//! `nudox-go-oracle`, so the user pointed `NUDOX_GO_ORACLE_BIN` at a prebuilt
//! binary they found in the checkout. That binary predated the fields the Rust
//! side had since started reading. Running the *current* oracle over a
//! four-declaration fixture produces `references: 2` and
//! `implements: Circle -> [Shape]` — the data was always there; the binary in
//! use could not produce it.
//!
//! # Why this needed a guard rather than a fix
//!
//! `oracle::Output`'s fields are `#[serde(default)]`, which is correct for
//! forward compatibility — a *newer* oracle adding a field must not break an
//! older reader. But the same attribute makes an *older* oracle omitting a
//! field indistinguishable from a package that genuinely has none: both
//! deserialize to an empty `Box<[_]>`. Every downstream surface then reports
//! the honest-looking answer for the wrong reason, and the sentinels this
//! workspace added (`not_recorded`) fire correctly while pointing at the wrong
//! cause.
//!
//! The subprocess boundary has no version handshake at all, so nothing else
//! could catch it: the Rust side cannot tell which oracle it just ran.

use nudox_languages::go::oracle::Output;

/// An oracle payload with no schema version at all — what every binary built
/// before the handshake emits.
const UNVERSIONED: &str = r#"{
  "module": { "path": "example.com/x", "dir": "/x", "goVersion": "1.21" },
  "packages": [
    {
      "importPath": "example.com/x",
      "name": "x",
      "decls": []
    }
  ]
}"#;

/// The same payload, declaring the version this build speaks.
fn versioned() -> String {
    UNVERSIONED.replacen(
        '{',
        &format!(
            "{{\n  \"schemaVersion\": {},",
            Output::REQUIRED_SCHEMA_VERSION
        ),
        1,
    )
}

/// An oracle that declares no schema version must be reported as stale.
///
/// This is the whole guard: the binary that produced the field report would
/// land here, and the reader would be told to rebuild it instead of being told
/// their Go package has no references.
#[test]
fn an_unversioned_oracle_payload_is_reported_as_stale() {
    let output: Output = serde_json::from_str(UNVERSIONED).expect(
        "an unversioned payload must still DESERIALIZE — the point is to \
         diagnose it, not to make old oracles unparseable",
    );

    let staleness = output
        .staleness()
        .expect("an oracle with no schema version is stale by definition");

    let rendered = staleness.to_string();
    assert!(
        rendered.contains("NUDOX_GO_ORACLE_BIN") || rendered.contains("nudox-go-oracle"),
        "the message must name the binary or the variable that points at it, \
         because rebuilding that binary is the entire fix: {rendered}",
    );
    assert!(
        rendered.contains("rebuild") || rendered.contains("out of date"),
        "the message must say the binary is out of date rather than describing \
         a data problem — the whole failure was mistaking one for the other: \
         {rendered}",
    );
}

/// A current oracle is not reported as stale.
///
/// Guards the direction that would make the check worthless: a staleness
/// signal that fires always is one every reader learns to ignore.
#[test]
fn a_current_oracle_payload_is_not_stale() {
    let output: Output =
        serde_json::from_str(&versioned()).expect("a versioned payload must deserialize");

    assert!(
        output.staleness().is_none(),
        "an oracle declaring the required schema version must not be flagged",
    );
}

/// An oracle NEWER than this build is not stale.
///
/// The asymmetry is the point: `#[serde(default)]` exists so a newer oracle
/// can add fields an older reader ignores. Rejecting it would trade one
/// false report for another.
#[test]
fn a_newer_oracle_payload_is_not_stale() {
    let newer = UNVERSIONED.replacen(
        '{',
        &format!(
            "{{\n  \"schemaVersion\": {},",
            Output::REQUIRED_SCHEMA_VERSION + 1
        ),
        1,
    );
    let output: Output = serde_json::from_str(&newer).expect("a newer payload must deserialize");

    assert!(
        output.staleness().is_none(),
        "a newer oracle is forward-compatible by construction; only an older \
         one under-reports",
    );
}

/// The committed Go oracle source must declare the schema the Rust reader
/// requires. A binary built from this tree that still spoke schema 0 was
/// how every Go package failed with "oracle is out of date" against a
/// lindsey build that already read schema 2.
#[test]
fn the_committed_go_oracle_source_declares_the_required_schema() {
    let src = include_str!("../../oracle/go/main.go");
    let declared = schema_const(src, "SchemaVersion")
        .expect("oracle/go/main.go must declare `const SchemaVersion = N`");
    assert_eq!(
        declared,
        Output::REQUIRED_SCHEMA_VERSION,
        "bump SchemaVersion in oracle/go/main.go in lockstep with \
         Output::REQUIRED_SCHEMA_VERSION — a wrap that ships this source's \
         binary is how lindsey.app stops depending on a scavenged schema-0 \
         prebuilt",
    );
}

fn schema_const(src: &str, name: &str) -> Option<u32> {
    let needle = format!("{name} = ");
    let idx = src.find(&needle)?;
    src[idx + needle.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}
