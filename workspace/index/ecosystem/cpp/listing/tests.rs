//! Adversarial corpus for the `ls-remote` listing parser.

use super::*;
use crate::ecosystem::cpp::version::CppVersion;

/// Convenience: parse a UTF-8 body and return `(tag_name, recorded_raw)` pairs.
fn parse(body: &str) -> Vec<(String, String)> {
    parse_ls_remote(body.as_bytes())
        .into_iter()
        .map(|listed| {
            let tag = match &listed.version {
                CppVersion::Tag(_)
                | CppVersion::Raw(_)
                | CppVersion::Date { .. }
                | CppVersion::Pseudo { .. } => {
                    // Recover the tag name from the `<tag>@<oid>` raw slot.
                    listed.raw.split('@').next().unwrap_or("").to_string()
                }
            };
            (tag, listed.raw.to_string())
        })
        .collect()
}

#[test]
fn annotated_tag_prefers_peeled_oid() {
    // Annotated tag: first line is the tag object, second (`^{}`) is the commit.
    let body = "\
1111111111111111111111111111111111111111\trefs/tags/v1.2.3\n\
2222222222222222222222222222222222222222\trefs/tags/v1.2.3^{}\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    // The peeled commit oid wins.
    assert_eq!(out[0].1, "v1.2.3@2222222222222222222222222222222222222222");
}

#[test]
fn lightweight_tag_uses_its_own_oid() {
    let body = "3333333333333333333333333333333333333333\trefs/tags/v2.0.0\n";
    let out = parse(body);
    assert_eq!(
        out,
        vec![(
            "v2.0.0".to_string(),
            "v2.0.0@3333333333333333333333333333333333333333".to_string()
        )]
    );
}

#[test]
fn heads_are_ignored() {
    let body = "\
4444444444444444444444444444444444444444\trefs/heads/main\n\
5555555555555555555555555555555555555555\trefs/tags/v1.0.0\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "v1.0.0");
}

#[test]
fn peeled_line_before_object_line() {
    // git normally emits the object line first, but tolerate the reverse.
    let body = "\
6666666666666666666666666666666666666666\trefs/tags/v3.0.0^{}\n\
7777777777777777777777777777777777777777\trefs/tags/v3.0.0\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    // Peeled oid recorded first and not clobbered by the later non-peel line.
    assert_eq!(out[0].1, "v3.0.0@6666666666666666666666666666666666666666");
}

#[test]
fn duplicate_tags_deduplicated() {
    let body = "\
8888888888888888888888888888888888888888\trefs/tags/v1.0.0\n\
9999999999999999999999999999999999999999\trefs/tags/v1.0.0\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
}

#[test]
fn malformed_lines_skipped() {
    let body = "\
not-a-valid-line-without-a-tab\n\
\trefs/tags/empty-oid\n\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1.0.0\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "v1.0.0");
}

#[test]
fn crlf_tolerated() {
    let body = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/tags/v4.5.6\r\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "v4.5.6");
}

#[test]
fn empty_input_yields_nothing() {
    assert!(parse("").is_empty());
    assert!(parse("\n\n\n").is_empty());
}

#[test]
fn tag_literally_named_peel_marker() {
    // A real tag named `^{}` (pathological but legal): the `after_prefix` equals
    // `^{}` with nothing before the suffix, so it is treated as a tag, not a peel.
    let body = "cccccccccccccccccccccccccccccccccccccccc\trefs/tags/^{}\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "^{}");
}

#[test]
fn invalid_utf8_yields_nothing() {
    let body = [0xff, 0xfe, 0x00];
    assert!(parse_ls_remote(&body).is_empty());
}

// ── Hostile-input hardening ───────────────────────────────────────────────

#[test]
fn unicode_tag_name_survives() {
    let body = "dddddddddddddddddddddddddddddddddddddddd\trefs/tags/日本語タグ\n";
    let out = parse(body);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "日本語タグ");
}

#[test]
fn enormous_number_of_tags_no_panic() {
    use std::fmt::Write as _;
    let mut body = String::new();
    for i in 0..30_000 {
        let _ = writeln!(body, "{i:040x}\trefs/tags/v0.0.{i}");
    }
    let out = parse_ls_remote(body.as_bytes());
    assert_eq!(out.len(), 30_000);
}

#[test]
fn empty_object_id_line_skipped_no_panic() {
    // Tab present but the object-id side is blank.
    let body = "\trefs/tags/v1.0.0\n";
    assert!(parse(body).is_empty());
}

#[test]
fn empty_reference_after_tab_no_panic() {
    let body = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t\n";
    assert!(parse(body).is_empty());
}

#[test]
fn many_tabs_on_one_line_no_panic() {
    let body = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1\t\t\t\textra\n";
    // split_once only splits on the FIRST tab, so the reference retains the
    // trailing tabs — it just won't match a clean tag name comparison; the
    // important property is no panic, and it's still recognised as a tag
    // ref (arbitrary trailing content included verbatim in the "tag name").
    let out = parse_ls_remote(body.as_bytes());
    assert_eq!(out.len(), 1);
}

#[test]
fn enormous_single_tag_name_no_panic() {
    let huge_name = "v".to_string() + &"0.".repeat(500_000) + "1";
    let body = format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/{huge_name}\n");
    let out = parse_ls_remote(body.as_bytes());
    assert_eq!(out.len(), 1);
}
