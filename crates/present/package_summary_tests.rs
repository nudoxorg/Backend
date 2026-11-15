//! A package summary reports identity, constraints and entry points, not raw source.
use crate::{Answer, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, encode_answer, product_view};
use backend_library::{
    InstalledPackageFacts, InstalledPackageFile, PackageCoordinate, ProductText, SurfaceReply,
};

fn file(path: &str, marker: &str, bytes: u64) -> InstalledPackageFile {
    let excerpt = format!(
        "// {marker}\n{}",
        "const interop = require('./interop');\n".repeat(80)
    );
    InstalledPackageFile {
        path: ProductText::new(path).expect("path"),
        digest: [9; 32],
        bytes,
        offset: 0,
        excerpt: ProductText::new(excerpt).expect("excerpt"),
        truncated: true,
    }
}

fn installed() -> Answer {
    let facts = InstalledPackageFacts {
        project: ProductText::new("/work/app").expect("project"),
        package: PackageCoordinate::parse("pkg:npm/widgets@2.4.1").expect("purl"),
        files: vec![
            file(
                "node_modules/widgets/package.json",
                "PACKAGE-JSON-BODY",
                9_000,
            ),
            file(
                "node_modules/widgets/dist/server/index.js",
                "INTEROP-BOILERPLATE",
                60_000,
            ),
            file("node_modules/widgets/README.md", "README-BODY", 7_000),
        ]
        .into_boxed_slice(),
        requirements: vec![
            ProductText::new("react ^19.0.0").expect("requirement"),
            ProductText::new("react-dom ^19.0.0").expect("requirement"),
        ]
        .into_boxed_slice(),
        engines: vec![ProductText::new("node >=20.9.0").expect("engine")].into_boxed_slice(),
    };
    Answer::Product(Box::new(product_view(&SurfaceReply::PackageInstalled(
        facts,
    ))))
}

fn encoded(detail: Detail) -> (usize, serde_json::Value) {
    let payload = encode_answer(&installed(), detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
        .expect("fits the response budget");
    (
        payload.bytes.len(),
        serde_json::from_slice(&payload.bytes).expect("JSON"),
    )
}

#[test]
fn a_summary_omits_raw_excerpts_and_stays_small() {
    let (summary_bytes, summary) = encoded(Detail::Summary);
    let (standard_bytes, standard) = encoded(Detail::Standard);
    println!("package bytes: summary {summary_bytes}, standard {standard_bytes}");
    assert!(summary_bytes < 3_000, "summary is {summary_bytes} bytes");
    assert!(
        standard_bytes > 8_000,
        "standard keeps the excerpts: {standard_bytes}"
    );
    let summary_text = summary.to_string();
    for raw in [
        "PACKAGE-JSON-BODY",
        "INTEROP-BOILERPLATE",
        "README-BODY",
        "require('./interop')",
    ] {
        assert!(!summary_text.contains(raw), "{raw} leaked into the summary");
        assert!(
            standard.to_string().contains(raw),
            "{raw} missing at standard"
        );
    }
    let note = summary["note"].as_str().expect("note");
    assert!(
        note.contains("omitted at summary") && note.contains("detail=standard"),
        "{note}"
    );
    assert!(
        standard["note"]
            .as_str()
            .is_some_and(|note| !note.contains("omitted at summary"))
    );
}

#[test]
fn a_summary_keeps_identity_constraints_and_entry_points() {
    let (_, summary) = encoded(Detail::Summary);
    let rows = summary["records"].as_array().expect("records");
    let titles = rows
        .iter()
        .map(|row| row["title"].as_str().expect("title"))
        .collect::<Vec<_>>();
    assert_eq!(titles[0], "pkg:npm/widgets@2.4.1");
    for kept in [
        "runtime constraint",
        "declared dependency",
        "node_modules/widgets/package.json",
        "node_modules/widgets/dist/server/index.js",
    ] {
        assert!(titles.contains(&kept), "{kept} in {titles:?}");
    }
    let entry = rows
        .iter()
        .find(|row| row["title"] == "node_modules/widgets/dist/server/index.js")
        .expect("entry point");
    let tag = entry["tags"][0].as_str().expect("observation tag");
    assert!(
        tag.contains("observed 60000 bytes")
            && tag.contains("digest")
            && tag.contains("partial source excerpt"),
        "{tag}"
    );
    assert_eq!(entry["tags"].as_array().map(Vec::len), Some(1));
    assert_eq!(rows[0]["operand"], "pkg:npm/widgets@2.4.1");
}

#[test]
fn the_summary_preview_leaves_the_excerpt_out_too() {
    let answer = installed();
    let summary = crate::markdown::answer_with_detail(
        &answer,
        Detail::Summary,
        None,
        crate::ContinuationTarget::CliOption,
    );
    assert!(!summary.contains("INTEROP-BOILERPLATE"), "{summary}");
    assert!(summary.contains("omitted at summary"), "{summary}");
    let standard = crate::markdown::answer_with_detail(
        &answer,
        Detail::Standard,
        None,
        crate::ContinuationTarget::CliOption,
    );
    assert!(standard.contains("INTEROP-BOILERPLATE"));
}
