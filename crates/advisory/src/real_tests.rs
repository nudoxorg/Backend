//! The real RUSTSEC-2025-0141 ("Bincode is unmaintained"), in both of the
//! forms advisory-db publishes: the Markdown document with fenced TOML front
//! matter, and its OSV export. Both must read as the same advisory: an
//! unmaintained notice, never a vulnerability, affecting bincode 1.3.3.

use super::*;

const MARKDOWN: &[u8] = include_bytes!("../fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md");
const OSV: &[u8] = include_bytes!("../fixtures/osv/RUSTSEC-2025-0141.json");

fn assert_bincode_unmaintained(advisory: &Advisory, form: &str) {
    assert_eq!(advisory.key.native.id, "RUSTSEC-2025-0141", "{form}: id");
    assert_eq!(
        advisory.summary.as_deref(),
        Some("Bincode is unmaintained"),
        "{form}: the title is the advisory's one line"
    );
    assert_eq!(
        advisory.categories.as_ref(),
        &[AdvisoryCategory::Unmaintained],
        "{form}: informational = \"unmaintained\" is a category, not a vulnerability"
    );
    assert_eq!(advisory.statuses().as_ref(), &[AdvisoryStatus::Unmaintained], "{form}: status");
    let range = &advisory.affected[0];
    assert_eq!(range.package.ecosystem, "cargo", "{form}: ecosystem");
    assert_eq!(range.package.name, "bincode", "{form}: package");
    assert_eq!(range_matches(range, "1.3.3"), Ok(true), "{form}: your bincode 1.3.3 is affected");
    assert_eq!(range_matches(range, "2.0.1"), Ok(true), "{form}: every release is");
}

#[test]
fn the_markdown_advisory_is_read_through_its_fence() {
    let advisory = parse_rustsec(MARKDOWN, 1).expect("the advisory-db Markdown form parses");
    assert_bincode_unmaintained(&advisory, "markdown");
}

#[test]
fn the_osv_export_names_the_same_unmaintained_advisory() {
    let advisory = parse_osv(OSV, 1).expect("the OSV form parses (crates.io, 0.0.0-0)");
    assert_bincode_unmaintained(&advisory, "osv");
}

#[test]
fn one_rustsec_document_is_a_partial_source_that_still_names_bincode() {
    let feed = AuthorityFeed::parse(AdvisorySource::RustSec, MARKDOWN, 7, None, None)
        .expect("single document feed");
    assert!(!feed.complete, "one advisory cannot vouch for every other package");
    let mut authority = AdvisoryAuthority::new(1_000);
    authority.apply(feed).expect("admit");
    let bincode = normalize_package("cargo", "bincode").expect("identity");
    let observation = authority.observe(&bincode, "1.3.3", false, false, 7, false);
    assert_eq!(observation.coverage, AdvisoryCoverage::Partial);
    let names = observation
        .advisories
        .iter()
        .map(|advisory| (advisory.key.native.id.as_str(), advisory.summary.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(names, [("RUSTSEC-2025-0141", Some("Bincode is unmaintained"))]);
    let surface = AdvisorySurfaceDto::from_observation(&observation);
    assert_eq!(surface[0].summary.as_deref(), Some("Bincode is unmaintained"));
    assert_eq!(surface[0].statuses.as_ref(), &[AdvisoryStatus::Unmaintained]);
    let toml = normalize_package("cargo", "toml").expect("identity");
    let clean = authority.observe(&toml, "0.8.23", false, false, 7, false);
    assert!(clean.advisories.is_empty());
    assert_eq!(clean.coverage, AdvisoryCoverage::Partial, "not checked is not clean");
}

#[test]
fn an_older_authority_file_without_titles_still_reopens() {
    let mut advisory = parse_rustsec(MARKDOWN, 1).expect("parse");
    advisory.summary = None;
    let bytes = serde_json::to_vec(&advisory).expect("encode");
    assert!(!String::from_utf8_lossy(&bytes).contains("summary"), "absent titles are not written");
    let back: Advisory = serde_json::from_slice(&bytes).expect("decode");
    assert_eq!(back, advisory);
}
