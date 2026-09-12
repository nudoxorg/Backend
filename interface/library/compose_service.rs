//! Defines compose service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the compose service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! `read` and `diff` as compositions of `show` and `outline`, so they need no store of their own.

use std::collections::BTreeMap;

use interface_documents::{Count, OutlineNode, ProjectionLimits};

use crate::{
    DiffChange, DiffRequest, Library, MAX_DIFF_SYMBOLS, MAX_READ_LOCATORS, PackageDiff, PageError,
    PageLocator, ReadPage, ReadRequest, ReadTerminal,
};

impl Library {
    /// Renders several pages in one call, each answered or refused on its own.
    #[must_use]
    pub fn read_pages(&self, request: &ReadRequest) -> ReadTerminal {
        let attempted = request.locators.len().min(MAX_READ_LOCATORS);
        let pages = request
            .locators
            .iter()
            .take(attempted)
            .map(|locator| ReadPage {
                locator: locator.clone(),
                page: self.show(locator, request.limits),
            })
            .collect();
        ReadTerminal {
            pages,
            dropped: Count(u32::try_from(request.locators.len().saturating_sub(attempted)).unwrap_or(u32::MAX)),
        }
    }

    /// Compares two versions of one package by outline path and kind, then by signature.
    ///
    /// # Errors
    ///
    /// Returns the exact failure of whichever outline could not be read.
    pub fn diff(&self, request: &DiffRequest) -> Result<PackageDiff, PageError> {
        let from = self.outline(&request.from)?;
        let to = self.outline(&request.to)?;
        let mut before: BTreeMap<String, &OutlineNode> = BTreeMap::new();
        let mut after: BTreeMap<String, &OutlineNode> = BTreeMap::new();
        let mut truncated = false;
        for (node, _) in from.walk() {
            if before.len() >= MAX_DIFF_SYMBOLS {
                truncated = true;
                break;
            }
            before.insert(diff_key(node), node);
        }
        for (node, _) in to.walk() {
            if after.len() >= MAX_DIFF_SYMBOLS {
                truncated = true;
                break;
            }
            after.insert(diff_key(node), node);
        }
        let mut changes = Vec::new();
        let mut unchanged = 0_u32;
        for (key, node) in &before {
            if !after.contains_key(key) {
                changes.push(DiffChange::Removed(node.symbol.clone()));
            }
        }
        for (key, node) in &after {
            if !before.contains_key(key) {
                changes.push(DiffChange::Added(node.symbol.clone()));
            }
        }
        for (key, old) in &before {
            let Some(new) = after.get(key) else {
                continue;
            };
            let signature_before = self
                .show(
                    &PageLocator::Entity {
                        package: request.from.clone(),
                        entity: old.symbol.entity,
                    },
                    ProjectionLimits::default(),
                )
                .map(|page| page.signature)
                .unwrap_or_default();
            let signature_after = self
                .show(
                    &PageLocator::Entity {
                        package: request.to.clone(),
                        entity: new.symbol.entity,
                    },
                    ProjectionLimits::default(),
                )
                .map(|page| page.signature)
                .unwrap_or_default();
            if signature_before.plain() == signature_after.plain() {
                unchanged = unchanged.saturating_add(1);
            } else {
                changes.push(DiffChange::Changed {
                    before: old.symbol.clone(),
                    after: new.symbol.clone(),
                    signature_before,
                    signature_after,
                });
            }
        }
        Ok(PackageDiff {
            from: request.from.clone(),
            to: request.to.clone(),
            changes: changes.into_boxed_slice(),
            unchanged: Count(unchanged),
            truncated,
        })
    }
}

/// The identity two versions are joined on: the root-relative path with its kind, never the
/// content key, which changes whenever a body changes.
fn diff_key(node: &OutlineNode) -> String {
    format!(
        "{}[{}]",
        node.symbol.address.as_address().path,
        interface_identity::KindTag::of(node.symbol.kind).as_str()
    )
}
