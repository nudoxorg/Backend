//! Defines source service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the source service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The source and related commands as this build answers them: both begin at the page the locator
//! names, so a locator that cannot be resolved fails exactly as `show` would.

use interface_documents::ProjectionLimits;

use crate::{
    Library, PageError, PageLocator, Related, RelatedRequest, SourceError, SourceRequest,
    SourceText,
};

impl Library {
    /// Reads one declaration's source excerpt.
    ///
    /// # Errors
    ///
    /// Returns the exact locate, span, root, or read failure.
    pub fn source(&self, request: &SourceRequest) -> Result<SourceText, SourceError> {
        let page = self
            .page(&request.locator, ProjectionLimits::default())
            .map_err(SourceError::Page)?;
        match (&page.source, self.card(page.symbol.package()).ok()) {
            (None, _) => Err(SourceError::NoSpan {
                symbol: page.symbol,
            }),
            (Some(_), None) | (Some(_), Some(_)) => Err(SourceError::NoSourceRoot {
                package: page.symbol.package().clone(),
            }),
        }
    }

    /// Lists the symbols related to one declaration.
    ///
    /// # Errors
    ///
    /// Returns the exact locate failure for the subject.
    pub fn related(&self, request: &RelatedRequest) -> Result<Related, PageError> {
        let page = self.page(&request.locator, ProjectionLimits::default())?;
        Ok(Related {
            symbol: page.symbol,
            rows: Box::new([]),
            graph: interface_search::Coverage::Unavailable {
                reason: interface_search::Unavailability::NoPackages,
            },
            semantic: interface_search::Coverage::Unavailable {
                reason: interface_search::Unavailability::NoPackages,
            },
        })
    }

    /// One page by locator, shared by the source and related commands.
    ///
    /// # Errors
    ///
    /// Returns the exact page failure.
    pub fn page(
        &self,
        locator: &PageLocator,
        limits: ProjectionLimits,
    ) -> Result<interface_documents::Page, PageError> {
        self.show(locator, limits)
    }
}
