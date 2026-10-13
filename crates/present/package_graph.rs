//! One bounded native graph selection policy for CLI and MCP output admission.
use crate::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use backend_library::{
    PackageGraphControl, PackageGraphPage, PackageGraphPageRequest, PackageGraphSourceKey,
};

#[derive(Debug, Eq, PartialEq)]
struct GraphBasis {
    root: [u8; 32],
    facts: [u8; 32],
    catalog: Option<[u8; 32]>,
    source: Option<PackageGraphSourceKey>,
}

/// Selects complete owner pages before rendering. Only an initial read can
/// shrink; a continuation always retains its issued exact limit and frontier.
pub struct PackageGraphPageSelection {
    request: PackageGraphPageRequest,
    basis: Option<GraphBasis>,
}

impl PackageGraphPageSelection {
    /// Preserves the caller's native operands and validates their row bound.
    pub fn new(request: &PackageGraphPageRequest) -> Result<Self, Fault> {
        request
            .admit()
            .map_err(|error| Fault::usage("package-graph", error.to_string()))?;
        Ok(Self {
            request: request.clone(),
            basis: None,
        })
    }

    /// Exact operands for the next complete producer read.
    pub fn request(&self) -> &PackageGraphPageRequest {
        &self.request
    }

    /// Pins roots, witness and selected authority across all candidate reads.
    pub fn admit_page(&mut self, page: &PackageGraphPage) -> Result<(), Fault> {
        page.admit_for(&self.request).map_err(|error| {
            Fault::from_client_error(
                &backend_client::ClientError::Protocol(error.to_string()),
                Operand::Whole,
            )
        })?;
        let basis = GraphBasis {
            root: page.view_root,
            facts: page.facts_witness,
            catalog: page.catalog_snapshot,
            source: page.source.clone(),
        };
        if self
            .basis
            .as_ref()
            .is_some_and(|original| original != &basis)
        {
            return Err(Fault::new(
                FaultSlug::Freshness,
                Operand::Whole,
                Cause::new(
                    CauseSlug::Moved,
                    "package graph changed while selecting a bounded page; restart the read without a cursor",
                ),
                Affordance::None,
            ));
        }
        self.basis = Some(basis);
        Ok(())
    }

    /// A resumed frontier, cancellation or minimum page cannot be resized.
    #[must_use]
    pub fn is_fixed_size(&self) -> bool {
        self.request.cursor.is_some()
            || self.request.control != PackageGraphControl::Continue
            || self.request.limit <= 1
    }

    /// Chooses a smaller first-page request only after an atomic output-budget
    /// refusal. No edge is removed from an admitted reply or resumed page.
    pub fn shrink_after(&mut self, fault: &Fault) -> bool {
        if fault.cause().slug() != CauseSlug::Oversized
            || self.basis.is_none()
            || self.is_fixed_size()
        {
            return false;
        }
        self.request.limit = (self.request.limit / 2).max(1);
        true
    }
}

/// Proves a CLI-only alternative fits before offering it on an oversized refusal.
/// Attachment metadata is supplied by the serving surface; this never dials,
/// starts, resolves paths, advances a cursor or substitutes an authority.
#[must_use]
pub fn package_graph_json_alternative(
    page: &PackageGraphPage,
    request: &PackageGraphPageRequest,
    detail: crate::Detail,
    context: Option<crate::StartupRecoveryContext>,
) -> Option<Affordance> {
    page.admit_for(request).ok()?;
    let context = context?;
    let payload =
        crate::encode_package_graph_json(page, detail, crate::DEFAULT_RESPONSE_BUDGET_BYTES)
            .ok()?;
    if payload.bytes.len().saturating_add(1) > crate::DEFAULT_RESPONSE_BUDGET_BYTES {
        return None;
    }
    let mut wire = payload.bytes.into_vec();
    wire.push(b'\n');
    let json = std::str::from_utf8(&wire).ok()?;
    if crate::cl100k_tokens(json).ok()? > crate::DEFAULT_RESPONSE_TOKEN_BUDGET {
        return None;
    }
    Some(Affordance::ReadPackageGraphJson {
        context,
        request: Box::new(request.clone()),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        PackageGraphCursor, PackageGraphDirection, PackageGraphKnowledge, PackageGraphPageTerminal,
        PackageGraphSourceAuthority, PackageReference,
    };
    fn request() -> PackageGraphPageRequest {
        PackageGraphPageRequest::new(
            PackageReference::parse("pkg:cargo/demo@1").expect("package"),
            PackageGraphDirection::Dependencies,
            None,
            64,
        )
        .expect("bounded request")
    }
    fn page(request: &PackageGraphPageRequest) -> PackageGraphPage {
        PackageGraphPage {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [7; 32],
            facts_witness: [8; 32],
            catalog_snapshot: None,
            package: request.package.clone(),
            direction: request.direction,
            source: Some(PackageGraphSourceKey::new(
                request.package.clone(),
                PackageGraphSourceAuthority::Local([1; 32]),
            )),
            limit: request.limit,
            knowledge: PackageGraphKnowledge::Known,
            rows: Box::new([]),
            terminal: PackageGraphPageTerminal::Complete,
        }
    }
    fn oversized() -> Fault {
        crate::oversized_fault(crate::BudgetExceeded {
            bytes: 50_000,
            budget: 49_152,
        })
    }
    #[test]
    fn adaptation_only_changes_an_initial_admitted_limit_and_pins_all_native_basis() {
        let original = request();
        let mut selection = PackageGraphPageSelection::new(&original).expect("initial selection");
        assert!(
            !selection.shrink_after(&oversized()),
            "no producer basis was admitted"
        );
        selection
            .admit_page(&page(&original))
            .expect("canonical initial basis");
        assert!(!selection.shrink_after(&Fault::usage("limit", "different failure")));
        assert!(selection.shrink_after(&oversized()));
        assert_eq!(selection.request().limit, 32);
        assert_eq!(selection.request().package, original.package);
        let smaller = page(selection.request());
        selection.admit_page(&smaller).expect("same exact graph");
        let mut moved = smaller;
        moved.facts_witness[0] ^= 1;
        assert_eq!(
            selection
                .admit_page(&moved)
                .expect_err("never silently mix graph roots")
                .cause()
                .slug(),
            CauseSlug::Moved
        );
    }
    #[test]
    fn resumes_cancellation_and_one_row_never_silently_resize() {
        let original = request();
        let source = page(&original).source;
        let resume = original.clone().with_cursor(PackageGraphCursor {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [7; 32],
            facts_witness: [8; 32],
            recipe: original.recipe(source.as_ref()),
            catalog_snapshot: None,
            source,
            limit: 64,
            after_edge_id: [0xff; 32],
            seal: [4; 32],
        });
        let mut selection = PackageGraphPageSelection::new(&resume).expect("native shape");
        selection
            .admit_page(&page(&resume))
            .expect("exact resumed limit");
        assert!(!selection.shrink_after(&oversized()));
        assert_eq!(selection.request(), &resume);
        let mut one = original.clone();
        one.limit = 1;
        let mut selection = PackageGraphPageSelection::new(&one).expect("one row");
        selection.admit_page(&page(&one)).expect("basis");
        assert!(!selection.shrink_after(&oversized()));
        let cancel = original.cancelled();
        let mut selection = PackageGraphPageSelection::new(&cancel).expect("cancel shape");
        let mut cancelled = page(&cancel);
        cancelled.terminal = PackageGraphPageTerminal::Cancelled;
        cancelled.knowledge = PackageGraphKnowledge::Unknown { reason: None };
        selection
            .admit_page(&cancelled)
            .expect("exact cancelled page");
        assert!(!selection.shrink_after(&oversized()));
    }
}
