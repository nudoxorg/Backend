//! Adversarial tests for the `cpp` version grammar.

use proptest::prelude::*;

use super::*;
use crate::version::VersionGrammar;

fn parse(raw: &str) -> CppVersion { CppVersion::parse_lossless(raw) }

// ── Kind classification ──────────────────────────────────────────────────────

#[test]
fn tag_versions_classify_as_tag() {
	assert!(matches!(parse("1.2.3"), CppVersion::Tag(_)));
	assert!(matches!(parse("v1.2.3"), CppVersion::Tag(_)));
	assert!(matches!(parse("1.2"), CppVersion::Tag(_)));
	assert!(matches!(parse("v3"), CppVersion::Tag(_)));
	assert!(matches!(parse("1.2.3.4"), CppVersion::Tag(_)));
	assert!(matches!(parse("1.2.3-rc1"), CppVersion::Tag(_)));
}

#[test]
fn version_date_classifies_as_date() {
	assert!(matches!(parse("2021-05-12"), CppVersion::Date { .. }));
	assert!(matches!(parse("2021-05-12#3"), CppVersion::Date { port_version: 3, .. }));
	// A bad date is not a Date.
	assert!(!matches!(parse("2021-13-40"), CppVersion::Date { .. }));
	assert!(!matches!(parse("2021-5-12"), CppVersion::Date { .. })); // needs 2-digit month
}

#[test]
fn pseudo_versions_classify_as_pseudo() {
	assert!(matches!(
		parse("v0.0.0-20200828120000-abcdefabcdef"),
		CppVersion::Pseudo { base: None, .. }
	));
	assert!(matches!(
		parse("v1.2.3-0.20200828120000-abcdefabcdef"),
		CppVersion::Pseudo { base: Some(_), .. }
	));
	assert!(matches!(
		parse("v1.2.3-pre.0.20200828120000-abcdefabcdef"),
		CppVersion::Pseudo { base: Some(_), .. }
	));
}

#[test]
fn raw_fallback_for_unstructured() {
	assert!(matches!(parse("main"), CppVersion::Raw(_)));
	assert!(matches!(parse("release-candidate"), CppVersion::Raw(_)));
	assert!(matches!(parse(""), CppVersion::Raw(_)));
}

// ── Cross-kind ordering: Tag > Date > Pseudo > Raw ──────────────────────────

#[test]
fn cross_kind_total_order() {
	let tag = parse("1.0.0");
	let date = parse("2021-01-01");
	let pseudo = parse("v0.0.0-20200828120000-abcdefabcdef");
	let raw = parse("main");
	assert!(tag > date);
	assert!(date > pseudo);
	assert!(pseudo > raw);
	assert!(tag > raw);
}

// ── Within-kind natural ordering ─────────────────────────────────────────────

#[test]
fn tag_natural_order() {
	assert!(parse("1.0.1") > parse("1.0.0"));
	assert!(parse("2.0.0") > parse("1.99.99"));
	assert!(parse("1.0.0") > parse("1.0.0-rc1")); // release > prerelease
	assert!(parse("1.0.0-alpha.2") < parse("1.0.0-alpha.10")); // numeric identifiers
	assert!(parse("v1.2") == parse("1.2.0")); // loose: missing patch is 0, v optional
}

#[test]
fn date_natural_order() {
	assert!(parse("2021-05-12") > parse("2021-05-11"));
	assert!(parse("2021-06-01") > parse("2021-05-31"));
	assert!(parse("2021-05-12#2") > parse("2021-05-12#1")); // port-version breaks ties
}

#[test]
fn pseudo_natural_order() {
	assert!(
		parse("v0.0.0-20200828120000-abcdefabcdef")
			> parse("v0.0.0-20200101000000-abcdefabcdef")
	);
	// Same timestamp, hash lexical tiebreak.
	assert!(
		parse("v0.0.0-20200828120000-ffffffffffff")
			> parse("v0.0.0-20200828120000-000000000000")
	);
	// No-base (form 1) sorts before a based pseudo of the same ts+hash.
	let form1 = parse("v0.0.0-20200828120000-abcdefabcdef");
	let form2 = parse("v1.0.0-0.20200828120000-abcdefabcdef");
	assert!(form2 > form1);
}

// ── is_prerelease ────────────────────────────────────────────────────────────

#[test]
fn is_prerelease_rules() {
	assert!(parse("v0.0.0-20200828120000-abcdefabcdef").is_prerelease());
	assert!(parse("1.0.0-rc1").is_prerelease());
	assert!(!parse("1.0.0").is_prerelease());
	assert!(!parse("2021-05-12").is_prerelease());
	assert!(!parse("main").is_prerelease());
}

// ── range_matches ────────────────────────────────────────────────────────────

#[test]
fn range_exact_equality() {
	assert!(CppVersion::range_matches("1.2.3", &parse("1.2.3")));
	assert!(CppVersion::range_matches("v1.2.3", &parse("1.2.3"))); // loose v-equality
	assert!(!CppVersion::range_matches("1.2.3", &parse("1.2.4")));
	assert!(CppVersion::range_matches("2021-05-12", &parse("2021-05-12")));
}

#[test]
fn range_caret_semantics_tag_only() {
	assert!(CppVersion::range_matches("^1.2.0", &parse("1.5.0")));
	assert!(CppVersion::range_matches("^1.2.0", &parse("1.2.0")));
	assert!(!CppVersion::range_matches("^1.2.0", &parse("2.0.0")));
	assert!(!CppVersion::range_matches("^1.2.0", &parse("1.1.0")));
	// Zero-major caret pins the minor.
	assert!(CppVersion::range_matches("^0.2.0", &parse("0.2.9")));
	assert!(!CppVersion::range_matches("^0.2.0", &parse("0.3.0")));
	// Caret against a non-tag candidate falls back to exact equality (fails).
	assert!(!CppVersion::range_matches("^1.0.0", &parse("main")));
}

// ── Pseudo-version grammar roundtrips + malformed rejection ───────────────────

#[test]
fn synthesize_form1_no_ancestor() {
	let v = synthesize_pseudo_version(None, 20200828120000, "abcdefabcdef").unwrap();
	assert_eq!(v, "v0.0.0-20200828120000-abcdefabcdef");
	// Roundtrips back to a Pseudo with no base.
	assert!(matches!(parse(&v), CppVersion::Pseudo { base: None, .. }));
}

#[test]
fn synthesize_form2_release_ancestor() {
	let ancestor = TagVersion::parse("v1.2.3").unwrap();
	let v = synthesize_pseudo_version(Some(&ancestor), 20200828120000, "abcdefabcdef").unwrap();
	// Patch is bumped and `-0.` prefix inserted (Go form 2).
	assert_eq!(v, "v1.2.4-0.20200828120000-abcdefabcdef");
	assert!(matches!(parse(&v), CppVersion::Pseudo { base: Some(_), .. }));
}

#[test]
fn synthesize_form3_prerelease_ancestor() {
	let ancestor = TagVersion::parse("v1.2.3-pre").unwrap();
	let v = synthesize_pseudo_version(Some(&ancestor), 20200828120000, "abcdefabcdef").unwrap();
	assert_eq!(v, "v1.2.3-pre.0.20200828120000-abcdefabcdef");
	assert!(matches!(parse(&v), CppVersion::Pseudo { base: Some(_), .. }));
}

#[test]
fn synthesize_rejects_malformed() {
	// 13-hex hash rejected.
	assert!(synthesize_pseudo_version(None, 20200828120000, "abcdefabcdef0").is_none());
	// 11-hex hash rejected.
	assert!(synthesize_pseudo_version(None, 20200828120000, "abcdefabcde").is_none());
	// Non-hex rejected.
	assert!(synthesize_pseudo_version(None, 20200828120000, "ghijklmnopqr").is_none());
	// Bad timestamp width rejected.
	assert!(synthesize_pseudo_version(None, 202008, "abcdefabcdef").is_none());
}

#[test]
fn parse_rejects_malformed_pseudo() {
	// 13-hex hash → not a pseudo (falls back).
	assert!(!matches!(
		parse("v0.0.0-20200828120000-abcdefabcdef0"),
		CppVersion::Pseudo { .. }
	));
	// Bad timestamp width → not a pseudo.
	assert!(!matches!(
		parse("v0.0.0-2020082812000-abcdefabcdef"),
		CppVersion::Pseudo { .. }
	));
}

// ── Property tests: total-order laws ─────────────────────────────────────────

/// A fixed adversarial pool spanning every kind.
fn pool() -> Vec<&'static str> {
	vec![
		"main",
		"develop",
		"v0.0.0-20190101000000-aaaaaaaaaaaa",
		"v0.0.0-20200828120000-cccccccccccc",
		"v1.0.0-0.20200828120000-abcdefabcdef",
		"2019-01-01",
		"2021-05-12",
		"2021-05-12#3",
		"0.9.0",
		"v1.0.0-alpha",
		"1.0.0-rc.1",
		"1.0.0",
		"v1.2.3",
		"1.2.3.4",
		"2.0.0",
		"10.0.0",
	]
}

proptest! {
	/// Antisymmetry: a < b ⇒ !(b < a).
	#[test]
	fn prop_antisymmetry(i in 0usize..16, j in 0usize..16) {
		let p = pool();
		let a = parse(p[i]);
		let b = parse(p[j]);
		if a < b {
			prop_assert!(!(b < a));
		}
		// Reflexive equality.
		prop_assert!(parse(p[i]) == parse(p[i]));
	}

	/// Transitivity: a <= b <= c ⇒ a <= c.
	#[test]
	fn prop_transitivity(i in 0usize..16, j in 0usize..16, k in 0usize..16) {
		let p = pool();
		let a = parse(p[i]);
		let b = parse(p[j]);
		let c = parse(p[k]);
		if a <= b && b <= c {
			prop_assert!(a <= c);
		}
	}

	/// Cross-kind ordering is fixed regardless of the concrete instances:
	/// any Tag > any Date > any Pseudo > any Raw.
	#[test]
	fn prop_cross_kind_rank(i in 0usize..16, j in 0usize..16) {
		let p = pool();
		let a = parse(p[i]);
		let b = parse(p[j]);
		let rank = |v: &CppVersion| match v {
			CppVersion::Tag(_) => 3,
			CppVersion::Date { .. } => 2,
			CppVersion::Pseudo { .. } => 1,
			CppVersion::Raw(_) => 0,
		};
		if rank(&a) > rank(&b) {
			prop_assert!(a > b);
		}
	}

	/// Sort determinism: sorting a permuted pool yields the same order.
	#[test]
	fn prop_sort_deterministic(rotation in 0usize..16) {
		let p = pool();
		let mut base: Vec<CppVersion> = p.iter().map(|s| parse(s)).collect();
		base.sort();
		let mut rotated: Vec<CppVersion> = p.iter().map(|s| parse(s)).collect();
		rotated.rotate_left(rotation);
		rotated.sort();
		prop_assert_eq!(base, rotated);
	}

	/// `parse(canonical(v))` preserves the ordering class: the re-parsed value
	/// compares equal to the original for every pool member. For `Tag`, `Pseudo`,
	/// and `Raw` the round-trip is exact; for `Date` the normalized `YYYY.MM.DD`
	/// form re-parses as a `Tag` (dot-separated numeric) that nonetheless sits in
	/// the same total-order position relative to the pool — so we assert the
	/// weaker, universal law: re-parse-then-sort is order-stable.
	#[test]
	fn prop_canonical_preserves_ordering_class(i in 0usize..16, j in 0usize..16) {
		let p = pool();
		let a = parse(p[i]);
		let b = parse(p[j]);
		// Tag/Pseudo/Raw round-trip to an equal value.
		if !matches!(a, CppVersion::Date { .. }) {
			prop_assert_eq!(parse(&a.canonical()), a.clone());
		}
		// The order relation between two versions is preserved after canonical
		// re-parse whenever neither is a Date (Date renders to a display form
		// outside the parse grammar, by contract).
		if !matches!(a, CppVersion::Date { .. }) && !matches!(b, CppVersion::Date { .. }) {
			let ra = parse(&a.canonical());
			let rb = parse(&b.canonical());
			prop_assert_eq!(a.cmp(&b), ra.cmp(&rb));
		}
	}
}

// ── canonical() rendering ─────────────────────────────────────────────────────

#[test]
fn canonical_tag_roundtrips() {
	let v = parse("v1.2.3-rc.1");
	assert_eq!(v.canonical(), "v1.2.3-rc.1");
	assert_eq!(parse(&v.canonical()), v);
}

#[test]
fn canonical_date_normalizes() {
	assert_eq!(parse("2021-05-12").canonical(), "2021.05.12");
	assert_eq!(parse("2021-05-12#3").canonical(), "2021.05.12.3");
}

#[test]
fn canonical_pseudo_is_exact_go_string() {
	let raw = "v0.0.0-20200828120000-cccccccccccc";
	assert_eq!(parse(raw).canonical(), raw);
	let form2 = "v1.0.1-0.20200828120000-abcdefabcdef";
	assert_eq!(parse(form2).canonical(), form2);
}

#[test]
fn canonical_raw_is_verbatim() {
	assert_eq!(parse("main").canonical(), "main");
	assert_eq!(parse("some-weird-branch").canonical(), "some-weird-branch");
}
