//! Paging over one complete references reply held in memory.
//!
//! The owner answers every use at once, so a page is a window over that reply.
//! A cursor names the offset of the next use and a digest of the whole listing:
//! a listing that changed between two pages is refused, never stitched.

use serde::{Deserialize, Serialize};

/// Prefix that tells a references cursor from an owner-issued one.
const PREFIX: &str = "rf1-";

/// Where one page sits in the complete listing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferencePageInfo {
    /// Zero-based position of this page's first use.
    pub offset: usize,
    /// Uses on this page.
    pub returned: usize,
    /// Uses in the complete listing.
    pub total: usize,
    /// Uses after this page.
    pub remaining: usize,
}

/// A decoded references cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReferenceCursor {
    pub(crate) offset: usize,
    pub(crate) digest: [u8; 32],
}

impl ReferenceCursor {
    pub(crate) fn encode(self) -> String {
        format!(
            "{PREFIX}{:08x}{}",
            self.offset,
            backend_library::encode_id(&self.digest)
        )
    }

    pub(crate) fn parse(token: &str) -> Option<Self> {
        let rest = token.strip_prefix(PREFIX)?;
        if rest.len() != 8 + 64 || !rest.is_char_boundary(8) {
            return None;
        }
        let (offset, digest) = rest.split_at(8);
        Some(Self {
            offset: usize::from_str_radix(offset, 16).ok()?,
            digest: backend_library::decode_id(digest).ok()?,
        })
    }
}

/// Whether `token` is a references cursor rather than an owner-issued one.
#[must_use]
pub fn is_reference_cursor(token: &str) -> bool {
    token.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::{Answer, CursorTarget, Detail, ProductView, encode_answer, product_view};
    use backend_library::{
        ProductText, ReferenceCoverage, ReferenceRecord, SemanticConfidence,
        SemanticDeclarationIdentity, SemanticLinkEvidence, SemanticLinkKind, SemanticLinkTarget,
        SemanticSourceSpan, SurfaceReply,
    };

    fn listing(count: usize) -> ProductView {
        let references = (0..count)
            .map(|index| ReferenceRecord {
                site_symbol: Some([7; 32]),
                source_policy: Default::default(),
                site: ProductText::new(format!(
                    "/work/app::semantic::{}::Caller{index}",
                    "ab".repeat(32)
                ))
                .expect("site"),
                target: SemanticLinkTarget::Local {
                    declaration: SemanticDeclarationIdentity {
                        family: [201; 16],
                        variant: [187; 16],
                    },
                },
                relation: SemanticLinkKind::TypeReference,
                evidence: SemanticLinkEvidence {
                    confidence: SemanticConfidence::Compiler,
                    source: Some(SemanticSourceSpan {
                        file: ProductText::new("components/Section.tsx").expect("file"),
                        start: u32::try_from(index).expect("small") * 10,
                        end: u32::try_from(index).expect("small") * 10 + 6,
                    }),
                },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        product_view(&SurfaceReply::References {
            target: ProductText::new("/work/app::semantic::target::Widget").expect("target"),
            coverage: ReferenceCoverage::Complete,
            references,
        })
    }

    fn operands(view: &ProductView) -> Vec<String> {
        view.records()
            .iter()
            .filter_map(|record| record.operand().map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_cursor_chain_reads_every_use_once_and_counts_what_remains() {
        let listing = listing(7);
        let mut cursor: Option<String> = None;
        let mut seen = Vec::new();
        let mut remaining = Vec::new();
        loop {
            let page = listing
                .reference_window(cursor.as_deref(), 3, CursorTarget::CliOption)
                .expect("page");
            seen.extend(operands(&page));
            remaining.push(page.reference_page().map(|info| info.remaining));
            match page.reference_cursor() {
                Some(next) => cursor = Some(next.token().to_owned()),
                None => break,
            }
        }
        assert_eq!(seen, operands(&listing), "every use once, in order");
        assert_eq!(remaining, [Some(4), Some(1), Some(0)]);
    }

    #[test]
    fn a_page_says_how_many_uses_remain_and_how_to_continue() {
        let page = listing(7)
            .reference_window(None, 3, CursorTarget::CliOption)
            .expect("page");
        let note = page.note().expect("note");
        assert!(note.contains("Uses 1-3 of 7; 4 more remain"), "{note}");
        assert!(note.contains("cursor"), "{note}");
        let info = page.reference_page().expect("position");
        assert_eq!(
            (info.offset, info.returned, info.total, info.remaining),
            (0, 3, 7, 4)
        );
        let rendered = crate::render::markdown::answer(&Answer::Product(Box::new(page)));
        assert!(rendered.contains("rf1-"), "the cursor is shown: {rendered}");
    }

    #[test]
    fn a_listing_that_fits_is_unchanged_and_carries_no_cursor() {
        let whole = listing(4);
        let page = whole
            .reference_window(None, 25, CursorTarget::CliOption)
            .expect("page");
        assert_eq!(
            page, whole,
            "the first page of a short listing is the listing"
        );
        assert!(page.reference_page().is_none());
        assert!(page.reference_cursor().is_none());
    }

    #[test]
    fn a_cursor_of_a_different_listing_is_stale() {
        let first = listing(7)
            .reference_window(None, 3, CursorTarget::CliOption)
            .expect("page");
        let token = first.reference_cursor().expect("more").token().to_owned();
        let fault = listing(8)
            .reference_window(Some(&token), 3, CursorTarget::CliOption)
            .expect_err("a changed listing");
        assert_eq!(fault.slug(), crate::FaultSlug::CursorMismatch);
        let fault = listing(7)
            .reference_window(Some("pc2-00"), 3, CursorTarget::CliOption)
            .expect_err("not a references cursor");
        assert!(fault.slug().is_usage());
    }

    #[test]
    fn a_page_size_never_changes_what_a_cursor_means() {
        let whole = listing(9);
        let first = whole
            .reference_window(None, 2, CursorTarget::CliOption)
            .expect("page");
        let token = first.reference_cursor().expect("more").token().to_owned();
        let wider = whole
            .reference_window(Some(&token), 6, CursorTarget::CliOption)
            .expect("page");
        assert_eq!(operands(&wider), operands(&whole)[2..8]);
        assert_eq!(wider.reference_page().map(|info| info.remaining), Some(1));
    }

    #[test]
    fn summary_rows_are_smaller_and_keep_every_proof_field() {
        let whole = listing(40);
        let answer = Answer::Product(Box::new(whole));
        let measure = |detail| {
            encode_answer(&answer, detail, None, crate::MAX_RESPONSE_BUDGET_BYTES)
                .expect("encoded")
                .bytes
        };
        let standard = measure(Detail::Standard);
        let summary = measure(Detail::Summary);
        let (before, after) = (standard.len() / 40, summary.len() / 40);
        println!("references bytes per row: before {before}, after {after}");
        assert!(
            after + 50 < before,
            "summary rows shed the restated prose: {before} -> {after}"
        );
        let value: serde_json::Value = serde_json::from_slice(&summary).expect("JSON");
        let row = &value["records"][3];
        assert_eq!(row["site_symbol"], backend_library::encode_id(&[7; 32]));
        assert_eq!(row["reference_details"]["relation"], "type-reference");
        assert_eq!(
            row["reference_details"]["evidence"]["confidence"],
            "compiler"
        );
        assert_eq!(
            row["reference_details"]["evidence"]["source"]["file"],
            "components/Section.tsx"
        );
        assert_eq!(row["reference_details"]["evidence"]["source"]["start"], 30);
        assert!(
            row["operand"]
                .as_str()
                .is_some_and(|operand| operand.ends_with("Caller3"))
        );
        let standard: serde_json::Value = serde_json::from_slice(&standard).expect("JSON");
        assert_eq!(
            standard["records"][3]["tags"][0], "type reference",
            "standard keeps the prose"
        );
    }
}
