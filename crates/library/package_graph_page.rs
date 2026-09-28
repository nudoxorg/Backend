//! Snapshot-bound, bounded package graph pages and opaque continuations.

use crate::package_graph::{
    CheckedPackageGraphFacts, DependencyFacts, PackageDependencyRecord, PackageGraphIndex,
    PackageGraphSourceAuthority, PackageGraphSourceKey, RegistryAuthorityId,
};
use crate::{PackageReference, ProductText};
use serde::{Deserialize, Serialize};

/// Maximum rows returned by one durable package graph read.
pub const MAX_PACKAGE_GRAPH_PAGE_ROWS: u16 = 128;
/// Maximum source authorities listed in an ambiguity response.
pub const MAX_PACKAGE_GRAPH_AUTHORITIES: usize = 64;
/// Schema for the root and facts-witness-bound package graph page contract.
pub const PACKAGE_GRAPH_PAGE_SCHEMA: u16 = 1;

/// Direction of one package graph read.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageGraphDirection {
    /// Packages declared by the selected package.
    Dependencies,
    /// Packages that declare a dependency on the selected package.
    Dependents,
}

/// Cooperative control for one package graph page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageGraphControl {
    /// Read or resume the page.
    Continue,
    /// Stop this exact graph read after snapshot admission.
    Cancel,
}

/// Opaque continuation for one exact package graph query.
///
/// A cursor commits to the selected view root, dependency witness, query
/// recipe, and exact source authority. The edge identity is the keyset
/// boundary for the next durable SQL page.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGraphCursor {
    /// Cursor schema.
    pub schema: u16,
    /// Immutable selected view root used by the first page.
    pub view_root: [u8; 32],
    /// Exact dependency-facts snapshot used by the first page.
    pub facts_witness: [u8; 32],
    /// Canonical query identity, including selected source authority.
    pub recipe: [u8; 32],
    /// Selected registry catalog snapshot when the page also carries catalog
    /// labels or health facts.
    pub catalog_snapshot: Option<[u8; 32]>,
    /// Exact source authority selected by a forward lookup, if applicable.
    pub source: Option<PackageGraphSourceKey>,
    /// Last edge identity returned to the caller.
    pub after_edge_id: [u8; 32],
}

/// Bounded request for a direct package dependency graph page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGraphPageRequest {
    /// Exact package coordinate at the center of the read.
    pub package: PackageReference,
    /// Direction around the center package.
    pub direction: PackageGraphDirection,
    /// Optional exact source authority for a forward lookup.
    pub authority: Option<PackageGraphSourceAuthority>,
    /// Expected selected registry catalog snapshot, when available.
    pub catalog_snapshot: Option<[u8; 32]>,
    /// Bounded page size.
    pub limit: u16,
    /// Continuation from the previous page.
    pub cursor: Option<PackageGraphCursor>,
    /// Cooperative execution control.
    pub control: PackageGraphControl,
}

impl PackageGraphPageRequest {
    /// Creates a first-page request after validating direction and page bound.
    pub fn new(
        package: PackageReference,
        direction: PackageGraphDirection,
        authority: Option<PackageGraphSourceAuthority>,
        limit: u16,
    ) -> Result<Self, PackageGraphPageError> {
        let request = Self {
            package,
            direction,
            authority,
            catalog_snapshot: None,
            limit,
            cursor: None,
            control: PackageGraphControl::Continue,
        };
        request.admit()?;
        Ok(request)
    }

    /// Attaches a continuation from the same package graph query.
    #[must_use]
    pub fn with_cursor(mut self, cursor: PackageGraphCursor) -> Self {
        self.catalog_snapshot = cursor.catalog_snapshot;
        self.cursor = Some(cursor);
        self
    }

    /// Binds this request to the selected registry catalog snapshot. A
    /// continuation must still name the exact snapshot committed by its
    /// cursor.
    pub fn bind_catalog_snapshot(
        mut self,
        catalog_snapshot: [u8; 32],
    ) -> Result<Self, PackageGraphPageError> {
        if self
            .catalog_snapshot
            .is_some_and(|expected| expected != catalog_snapshot)
        {
            return Err(PackageGraphPageError::StaleCursor);
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.catalog_snapshot != Some(catalog_snapshot))
        {
            return Err(PackageGraphPageError::StaleCursor);
        }
        self.catalog_snapshot = Some(catalog_snapshot);
        Ok(self)
    }

    /// Rechecks the page bound and direction-specific authority at a process
    /// boundary, including requests decoded from the wire.
    pub fn admit(&self) -> Result<(), PackageGraphPageError> {
        if self.limit == 0 || self.limit > MAX_PACKAGE_GRAPH_PAGE_ROWS {
            return Err(PackageGraphPageError::PageBound);
        }
        if self.direction == PackageGraphDirection::Dependents && self.authority.is_some() {
            return Err(PackageGraphPageError::AuthorityNotApplicable);
        }
        if let Some(cursor) = &self.cursor {
            if cursor.schema != PACKAGE_GRAPH_PAGE_SCHEMA
                || cursor.catalog_snapshot != self.catalog_snapshot
                || cursor.source.as_ref().is_some_and(|source| {
                    source.coordinate != self.package
                        || self.direction != PackageGraphDirection::Dependencies
                        || self
                            .authority
                            .is_some_and(|authority| authority != source.authority)
                })
            {
                return Err(PackageGraphPageError::CursorMismatch);
            }
        }
        Ok(())
    }

    /// Cancels this exact package graph query after snapshot admission.
    #[must_use]
    pub const fn cancelled(mut self) -> Self {
        self.control = PackageGraphControl::Cancel;
        self
    }

    /// Returns the canonical query identity after resolving the forward
    /// source authority. A forward cursor cannot be replayed against another
    /// mirror that publishes the same coordinate.
    #[must_use]
    pub fn recipe(&self, selected_source: Option<&PackageGraphSourceKey>) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.package-graph.page.v1\0");
        hash_identity_part(&mut hasher, self.package.as_str().as_bytes());
        match self.catalog_snapshot {
            Some(snapshot) => {
                hasher.update(&[1]);
                hasher.update(&snapshot);
            }
            None => {
                hasher.update(&[0]);
            }
        }
        hasher.update(&[match self.direction {
            PackageGraphDirection::Dependencies => 0,
            PackageGraphDirection::Dependents => 1,
        }]);
        hasher.update(&self.limit.to_be_bytes());
        if let Some(authority) = self.authority {
            hasher.update(&[1, authority.kind_tag() as u8]);
            hasher.update(&authority.id_bytes());
        } else {
            hasher.update(&[0]);
        }
        if let Some(source) = selected_source {
            hasher.update(&[1]);
            hash_identity_part(&mut hasher, source.coordinate.as_str().as_bytes());
            hasher.update(&[source.authority.kind_tag() as u8]);
            hasher.update(&source.authority.id_bytes());
        } else {
            hasher.update(&[0]);
        }
        *hasher.finalize().as_bytes()
    }
}

fn hash_identity_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
}

/// Why a package graph page has no rows, or which rows it contains.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum PackageGraphKnowledge {
    /// The selected authority answered; an empty complete page means known-empty.
    Known,
    /// No dependency answer was recorded, or the coordinate is absent.
    Unknown { reason: Option<ProductText> },
    /// The source authority could not answer the dependency request.
    Unavailable { reason: ProductText },
    /// Some sources answered, but other sources in the selected graph
    /// snapshot did not. Rows remain useful as positive evidence while the
    /// reason makes the incomplete coverage explicit.
    Partial {
        reason: ProductText,
        unavailable: bool,
    },
    /// Several authorities publish this coordinate and must be selected.
    Ambiguous {
        sources: Box<[PackageGraphSourceKey]>,
    },
}

/// Completion of one bounded package graph page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "cursor", rename_all = "kebab-case")]
pub enum PackageGraphPageTerminal {
    /// The answer is complete. `Known` plus no rows means known-empty.
    Complete,
    /// Another page is available at the same graph roots and authority.
    More(PackageGraphCursor),
    /// The caller cancelled this exact request.
    Cancelled,
}

/// One immutable, bounded package graph page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGraphPage {
    /// Contract schema.
    pub schema: u16,
    /// Selected immutable view root.
    pub view_root: [u8; 32],
    /// Exact dependency-facts snapshot at that root.
    pub facts_witness: [u8; 32],
    /// Selected registry catalog snapshot when catalog-backed package facts
    /// are included alongside this graph page.
    pub catalog_snapshot: Option<[u8; 32]>,
    /// Center package and requested direction.
    pub package: PackageReference,
    /// Direction around the center package.
    pub direction: PackageGraphDirection,
    /// Exact source selected for a forward lookup. Reverse rows carry their
    /// own exact source keys, so the page-level source remains absent there.
    pub source: Option<PackageGraphSourceKey>,
    /// Known, unknown, unavailable, or ambiguous fact state.
    pub knowledge: PackageGraphKnowledge,
    /// Dependency edges in stable edge-id order.
    pub rows: Box<[PackageDependencyRecord]>,
    /// Whether this page completed, has a continuation, or was cancelled.
    pub terminal: PackageGraphPageTerminal,
}

impl PackageGraphPage {
    /// Verifies the page's bounded and identity-preserving shape before it
    /// crosses a product or transport boundary.
    pub fn admit(&self) -> Result<(), PackageGraphPageError> {
        if self.schema != PACKAGE_GRAPH_PAGE_SCHEMA
            || self.rows.len() > usize::from(MAX_PACKAGE_GRAPH_PAGE_ROWS)
            || self
                .rows
                .windows(2)
                .any(|pair| pair[0].facts_version >= pair[1].facts_version)
            || self
                .rows
                .iter()
                .any(|row| row.facts_version != row.recomputed_version())
        {
            return Err(PackageGraphPageError::PageShape);
        }
        match self.direction {
            PackageGraphDirection::Dependencies => {
                if let Some(source) = &self.source {
                    if source.coordinate != self.package
                        || self.rows.iter().any(|row| {
                            row.source != source.coordinate
                                || row.source_authority != source.authority
                        })
                    {
                        return Err(PackageGraphPageError::PageShape);
                    }
                } else if !self.rows.is_empty() {
                    return Err(PackageGraphPageError::PageShape);
                }
                match &self.knowledge {
                    PackageGraphKnowledge::Known if self.source.is_none() => {
                        return Err(PackageGraphPageError::PageShape);
                    }
                    PackageGraphKnowledge::Unknown { .. } if !self.rows.is_empty() => {
                        return Err(PackageGraphPageError::PageShape);
                    }
                    PackageGraphKnowledge::Unavailable { .. }
                        if self.source.is_none() || !self.rows.is_empty() =>
                    {
                        return Err(PackageGraphPageError::PageShape);
                    }
                    PackageGraphKnowledge::Partial { .. } => {
                        return Err(PackageGraphPageError::PageShape);
                    }
                    PackageGraphKnowledge::Ambiguous { sources } => {
                        if !self.rows.is_empty()
                            || self.source.is_some()
                            || sources.len() < 2
                            || sources.len() > MAX_PACKAGE_GRAPH_AUTHORITIES
                            || sources
                                .iter()
                                .any(|source| source.coordinate != self.package)
                            || sources.windows(2).any(|pair| pair[0] >= pair[1])
                        {
                            return Err(PackageGraphPageError::PageShape);
                        }
                    }
                    _ => {}
                }
            }
            PackageGraphDirection::Dependents => {
                if self.source.is_some() {
                    return Err(PackageGraphPageError::PageShape);
                }
                if matches!(&self.knowledge, PackageGraphKnowledge::Ambiguous { .. }) {
                    return Err(PackageGraphPageError::PageShape);
                }
                if matches!(
                    &self.knowledge,
                    PackageGraphKnowledge::Unknown { .. }
                        | PackageGraphKnowledge::Unavailable { .. }
                ) && !self.rows.is_empty()
                {
                    return Err(PackageGraphPageError::PageShape);
                }
                match &self.package {
                    PackageReference::Purl(target) => {
                        if let Some(ecosystem) = target.package_type().registry() {
                            if self.rows.iter().any(|row| {
                                row.target.ecosystem != ecosystem
                                    || row.target.name.as_str() != target.lineage_name()
                                    || row.target.resolved.as_ref().is_some_and(|resolved| {
                                        resolved.as_str() != self.package.as_str()
                                    })
                            }) {
                                return Err(PackageGraphPageError::PageShape);
                            }
                        } else if !self.rows.is_empty() {
                            return Err(PackageGraphPageError::PageShape);
                        }
                    }
                    PackageReference::Local(_) if !self.rows.is_empty() => {
                        return Err(PackageGraphPageError::PageShape);
                    }
                    PackageReference::Local(_) => {}
                }
            }
        }
        match &self.terminal {
            PackageGraphPageTerminal::More(cursor)
                if self.rows.is_empty()
                    || !matches!(
                        &self.knowledge,
                        PackageGraphKnowledge::Known | PackageGraphKnowledge::Partial { .. }
                    )
                    || cursor.schema != PACKAGE_GRAPH_PAGE_SCHEMA
                    || cursor.view_root != self.view_root
                    || cursor.facts_witness != self.facts_witness
                    || cursor.catalog_snapshot != self.catalog_snapshot
                    || cursor.source != self.source
                    || self.rows.last().map(|row| row.facts_version)
                        != Some(cursor.after_edge_id) =>
            {
                Err(PackageGraphPageError::PageShape)
            }
            PackageGraphPageTerminal::Cancelled
                if !self.rows.is_empty()
                    || !matches!(
                        &self.knowledge,
                        PackageGraphKnowledge::Unknown { reason: None }
                    ) =>
            {
                Err(PackageGraphPageError::PageShape)
            }
            _ => Ok(()),
        }
    }

    /// Validates a decoded product reply against the exact request which
    /// produced it, including authority selection and continuation progress.
    pub fn admit_for(
        &self,
        request: &PackageGraphPageRequest,
    ) -> Result<(), PackageGraphPageError> {
        request.admit()?;
        self.admit()?;
        if self.package != request.package || self.direction != request.direction {
            return Err(PackageGraphPageError::PageShape);
        }
        if request.control == PackageGraphControl::Cancel {
            if self.terminal != PackageGraphPageTerminal::Cancelled {
                return Err(PackageGraphPageError::PageShape);
            }
        } else if matches!(&self.terminal, PackageGraphPageTerminal::Cancelled) {
            return Err(PackageGraphPageError::PageShape);
        }
        if request
            .catalog_snapshot
            .is_some_and(|snapshot| Some(snapshot) != self.catalog_snapshot)
        {
            return Err(PackageGraphPageError::CursorMismatch);
        }
        match request.direction {
            PackageGraphDirection::Dependencies => {
                if request.authority.is_some()
                    && matches!(&self.knowledge, PackageGraphKnowledge::Ambiguous { .. })
                {
                    return Err(PackageGraphPageError::CursorMismatch);
                }
                if request.authority.is_some_and(|authority| {
                    self.source
                        .as_ref()
                        .is_some_and(|source| source.authority != authority)
                }) {
                    return Err(PackageGraphPageError::CursorMismatch);
                }
            }
            PackageGraphDirection::Dependents if self.source.is_some() => {
                return Err(PackageGraphPageError::PageShape);
            }
            PackageGraphDirection::Dependents => {}
        }
        if let Some(cursor) = &request.cursor {
            if cursor.schema != PACKAGE_GRAPH_PAGE_SCHEMA
                || cursor.view_root != self.view_root
                || cursor.facts_witness != self.facts_witness
                || cursor.catalog_snapshot != self.catalog_snapshot
                || cursor.source != self.source
                || cursor.recipe != request.recipe(self.source.as_ref())
                || self
                    .rows
                    .first()
                    .is_some_and(|row| row.facts_version <= cursor.after_edge_id)
            {
                return Err(PackageGraphPageError::CursorMismatch);
            }
        }
        if let PackageGraphPageTerminal::More(cursor) = &self.terminal
            && cursor.recipe != request.recipe(self.source.as_ref())
        {
            return Err(PackageGraphPageError::CursorMismatch);
        }
        Ok(())
    }

    /// Authenticates this durable page against the exact resident checked
    /// facts snapshot that was used to synchronize its projection.
    ///
    /// The forward path seeks directly to the selected canonical source and
    /// edge cursor. The reverse path reads only the matching sorted postings
    /// from `index`, bounded to one page plus its continuation probe.
    pub fn admit_against_checked_facts(
        &self,
        request: &PackageGraphPageRequest,
        expected_view_root: [u8; 32],
        facts: &CheckedPackageGraphFacts,
        index: &PackageGraphIndex,
    ) -> Result<(), PackageGraphPageError> {
        request.admit()?;
        self.admit_for(request)?;
        if !index.is_bound_to(facts) {
            return Err(PackageGraphPageError::FactsMismatch);
        }
        let expected = expected_page(request, expected_view_root, facts, index)?;
        if self != &expected {
            return Err(PackageGraphPageError::FactsMismatch);
        }
        Ok(())
    }
}

enum CheckedSourceSelection<'a> {
    Missing,
    Exact(
        &'a PackageGraphSourceKey,
        &'a DependencyFacts<Box<[PackageDependencyRecord]>>,
    ),
    Ambiguous(Box<[PackageGraphSourceKey]>),
}

fn expected_page(
    request: &PackageGraphPageRequest,
    view_root: [u8; 32],
    facts: &CheckedPackageGraphFacts,
    index: &PackageGraphIndex,
) -> Result<PackageGraphPage, PackageGraphPageError> {
    let facts_witness = facts.witness();
    if let Some(cursor) = &request.cursor
        && (cursor.schema != PACKAGE_GRAPH_PAGE_SCHEMA
            || cursor.view_root != view_root
            || cursor.facts_witness != facts_witness
            || cursor.catalog_snapshot != request.catalog_snapshot)
    {
        return Err(PackageGraphPageError::StaleCursor);
    }

    let selection = if request.direction == PackageGraphDirection::Dependencies {
        Some(select_checked_source(request, facts)?)
    } else {
        None
    };
    let selected_source = match &selection {
        Some(CheckedSourceSelection::Exact(source, _)) => Some((*source).clone()),
        _ => None,
    };
    if let Some(cursor) = &request.cursor
        && (cursor.source != selected_source
            || cursor.recipe != request.recipe(selected_source.as_ref()))
    {
        return Err(PackageGraphPageError::CursorMismatch);
    }

    if request.control == PackageGraphControl::Cancel {
        return Ok(PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root,
            facts_witness,
            catalog_snapshot: request.catalog_snapshot,
            package: request.package.clone(),
            direction: request.direction,
            source: selected_source,
            knowledge: PackageGraphKnowledge::Unknown { reason: None },
            rows: Box::new([]),
            terminal: PackageGraphPageTerminal::Cancelled,
        });
    }

    let (source, knowledge, rows, more) = match request.direction {
        PackageGraphDirection::Dependencies => match selection.expect("forward selection exists") {
            CheckedSourceSelection::Missing => (
                None,
                PackageGraphKnowledge::Unknown {
                    reason: Some(product_text(
                        "no dependency source is recorded for this package",
                    )),
                },
                Vec::new(),
                false,
            ),
            CheckedSourceSelection::Ambiguous(sources) => (
                None,
                PackageGraphKnowledge::Ambiguous { sources },
                Vec::new(),
                false,
            ),
            CheckedSourceSelection::Exact(source, state) => match state {
                DependencyFacts::Known(source_rows) => {
                    let after = request.cursor.as_ref().map(|cursor| cursor.after_edge_id);
                    let start = after.map_or(0, |after| {
                        source_rows.partition_point(|row| row.facts_version <= after)
                    });
                    let page_bound = usize::from(request.limit).saturating_add(1);
                    let selected = source_rows
                        .get(start..)
                        .unwrap_or_default()
                        .iter()
                        .take(page_bound)
                        .cloned()
                        .collect::<Vec<_>>();
                    let more = selected.len() > usize::from(request.limit);
                    let rows = selected
                        .into_iter()
                        .take(usize::from(request.limit))
                        .collect();
                    (
                        Some(source.clone()),
                        PackageGraphKnowledge::Known,
                        rows,
                        more,
                    )
                }
                DependencyFacts::Unknown(reason) => (
                    Some(source.clone()),
                    PackageGraphKnowledge::Unknown {
                        reason: Some(reason.clone()),
                    },
                    Vec::new(),
                    false,
                ),
                DependencyFacts::Unavailable(reason) => (
                    Some(source.clone()),
                    PackageGraphKnowledge::Unavailable {
                        reason: reason.clone(),
                    },
                    Vec::new(),
                    false,
                ),
            },
        },
        PackageGraphDirection::Dependents => {
            let (rows, more) = index.dependent_edges_page(
                facts.facts(),
                &request.package,
                request.cursor.as_ref().map(|cursor| cursor.after_edge_id),
                request.limit,
            );
            let rows = rows.into_iter().cloned().collect::<Vec<_>>();
            let knowledge = match &request.package {
                PackageReference::Local(_) => PackageGraphKnowledge::Unknown {
                    reason: Some(product_text(
                        "dependent lookup requires a versioned registry package coordinate",
                    )),
                },
                PackageReference::Purl(target) if target.package_type().registry().is_none() => {
                    checked_reverse_knowledge(facts.facts())
                }
                PackageReference::Purl(_) => {
                    let coverage = checked_reverse_knowledge(facts.facts());
                    match coverage {
                        PackageGraphKnowledge::Unknown {
                            reason: Some(reason),
                        } if !rows.is_empty() || more => PackageGraphKnowledge::Partial {
                            reason,
                            unavailable: false,
                        },
                        PackageGraphKnowledge::Unavailable { reason }
                            if !rows.is_empty() || more =>
                        {
                            PackageGraphKnowledge::Partial {
                                reason,
                                unavailable: true,
                            }
                        }
                        other => other,
                    }
                }
            };
            (None, knowledge, rows, more)
        }
    };

    let terminal = if more {
        let after_edge_id = rows
            .last()
            .map(|row| row.facts_version)
            .ok_or(PackageGraphPageError::FactsMismatch)?;
        PackageGraphPageTerminal::More(PackageGraphCursor {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root,
            facts_witness,
            recipe: request.recipe(source.as_ref()),
            catalog_snapshot: request.catalog_snapshot,
            source: source.clone(),
            after_edge_id,
        })
    } else {
        PackageGraphPageTerminal::Complete
    };
    Ok(PackageGraphPage {
        schema: PACKAGE_GRAPH_PAGE_SCHEMA,
        view_root,
        facts_witness,
        catalog_snapshot: request.catalog_snapshot,
        package: request.package.clone(),
        direction: request.direction,
        source,
        knowledge,
        rows: rows.into_boxed_slice(),
        terminal,
    })
}

fn select_checked_source<'a>(
    request: &PackageGraphPageRequest,
    facts: &'a CheckedPackageGraphFacts,
) -> Result<CheckedSourceSelection<'a>, PackageGraphPageError> {
    let all_facts = facts.facts();
    let coordinate = request.package.as_str();
    let start = all_facts.partition_point(|(source, _)| source.coordinate.as_str() < coordinate);
    let end = start
        + all_facts[start..]
            .iter()
            .take_while(|(source, _)| source.coordinate.as_str() == coordinate)
            .count();
    let sources = &all_facts[start..end];
    if let Some(cursor_source) = request
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.source.as_ref())
    {
        return sources
            .iter()
            .find(|(source, _)| *source == *cursor_source)
            .map(|(source, state)| CheckedSourceSelection::Exact(source, state))
            .ok_or(PackageGraphPageError::StaleCursor);
    }
    if let Some(authority) = request.authority {
        return Ok(sources
            .iter()
            .find(|(source, _)| source.authority == authority)
            .map_or(CheckedSourceSelection::Missing, |(source, state)| {
                CheckedSourceSelection::Exact(source, state)
            }));
    }
    if sources.len() > MAX_PACKAGE_GRAPH_AUTHORITIES {
        return Err(PackageGraphPageError::AuthorityFanout);
    }
    match sources {
        [] => Ok(CheckedSourceSelection::Missing),
        [(source, state)] => Ok(CheckedSourceSelection::Exact(source, state)),
        many => {
            let mut keys = many
                .iter()
                .map(|(source, _)| source.clone())
                .collect::<Vec<_>>();
            keys.sort_unstable();
            Ok(CheckedSourceSelection::Ambiguous(keys.into_boxed_slice()))
        }
    }
}

fn checked_reverse_knowledge(
    facts: &[crate::package_graph::PackageDependencySourceFacts],
) -> PackageGraphKnowledge {
    facts
        .iter()
        .find_map(|(_, state)| match state {
            DependencyFacts::Unknown(reason) => Some(PackageGraphKnowledge::Unknown {
                reason: Some(reason.clone()),
            }),
            DependencyFacts::Unavailable(reason) => Some(PackageGraphKnowledge::Unavailable {
                reason: reason.clone(),
            }),
            DependencyFacts::Known(_) => None,
        })
        .unwrap_or(PackageGraphKnowledge::Known)
}

fn product_text(value: &str) -> ProductText {
    ProductText::new(value).expect("static package graph reason is valid")
}

/// Semantic rejection of a package graph page request or cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageGraphPageError {
    /// Page size was zero or exceeded the fixed bound.
    PageBound,
    /// A source authority was provided for a reverse lookup.
    AuthorityNotApplicable,
    /// A copyable source authority selector was malformed.
    AuthoritySelector,
    /// The coordinate has more authorities than the bounded chooser allows.
    AuthorityFanout,
    /// Cursor schema, recipe, or key did not match the request.
    CursorMismatch,
    /// The selected root or facts witness changed after the first page.
    StaleCursor,
    /// The reply violated the package graph page contract.
    PageShape,
    /// The durable page differs from the authenticated resident facts.
    FactsMismatch,
}

impl std::fmt::Display for PackageGraphPageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PageBound => "package graph page exceeds its row bound",
            Self::AuthorityNotApplicable => "source authority applies only to dependency reads",
            Self::AuthoritySelector => {
                "package graph authority must be a source tag and 64 hex digits"
            }
            Self::AuthorityFanout => {
                "package coordinate has too many source authorities to list safely"
            }
            Self::CursorMismatch => "package graph cursor does not match its request",
            Self::StaleCursor => {
                "package graph changed since this cursor was issued; restart the read"
            }
            Self::PageShape => "package graph page has inconsistent identities or state",
            Self::FactsMismatch => {
                "package graph page differs from its authenticated resident facts"
            }
        })
    }
}

impl std::error::Error for PackageGraphPageError {}

impl PackageGraphSourceAuthority {
    /// Stable copyable spelling for product rows and the `package-graph`
    /// command's `--authority` option.
    #[must_use]
    pub fn selector(self) -> String {
        if self == Self::Unattributed {
            return "unattributed".to_owned();
        }
        let kind = match self {
            Self::Registry(_) => "registry",
            Self::Forge(_) => "forge",
            Self::Archive(_) => "archive",
            Self::Local(_) => "local",
            Self::Unattributed => return "unattributed".to_owned(),
        };
        let mut value = String::with_capacity(73);
        value.push_str(kind);
        value.push(':');
        for byte in self.id_bytes() {
            use std::fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
        }
        value
    }

    /// Parses the stable authority spelling emitted by [`Self::selector`].
    pub fn parse_selector(value: &str) -> Result<Self, PackageGraphPageError> {
        if value == "unattributed" {
            return Ok(Self::Unattributed);
        }
        let (kind, encoded) = value
            .split_once(':')
            .ok_or(PackageGraphPageError::AuthoritySelector)?;
        if encoded.len() != 64 {
            return Err(PackageGraphPageError::AuthoritySelector);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (parse_hex(pair[0]).ok_or(PackageGraphPageError::AuthoritySelector)?
                << 4)
                | parse_hex(pair[1]).ok_or(PackageGraphPageError::AuthoritySelector)?;
        }
        match kind {
            "registry" => Ok(Self::Registry(RegistryAuthorityId::from_configured_source(
                bytes,
            ))),
            "forge" => Ok(Self::Forge(bytes)),
            "archive" => Ok(Self::Archive(bytes)),
            "local" => Ok(Self::Local(bytes)),
            _ => Err(PackageGraphPageError::AuthoritySelector),
        }
    }
}

fn parse_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CheckedPackageGraphFacts, DependencyAuthority, DependencyEvidence, DependencyFacts,
        DependencyScope, PackageDependencySourceFacts, PackageDependencyTarget, PackageGraphIndex,
        RegistryEcosystem,
    };

    fn package() -> PackageReference {
        PackageReference::parse("pkg:cargo/demo@1.0.0").expect("valid package")
    }

    fn source() -> PackageGraphSourceKey {
        PackageGraphSourceKey::new(
            package(),
            PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
                [0x18; 32],
            )),
        )
    }

    fn edge(
        source: &PackageGraphSourceKey,
        target_name: &str,
        requirement: &str,
        resolved: Option<&str>,
    ) -> PackageDependencyRecord {
        PackageDependencyRecord::new_with_source_authority(
            source.coordinate.clone(),
            source.authority,
            PackageDependencyTarget::new(
                RegistryEcosystem::Cargo,
                target_name,
                requirement,
                resolved.map(|value| PackageReference::parse(value).expect("resolved package")),
            )
            .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [0x28; 32],
                provenance: [0x38; 32],
            },
        )
    }

    fn checked_graph() -> (
        CheckedPackageGraphFacts,
        PackageGraphIndex,
        PackageGraphSourceKey,
        Vec<PackageDependencyRecord>,
    ) {
        let source = source();
        let rows = vec![
            edge(&source, "serde", "^1", None),
            edge(&source, "serde", "^1", Some("pkg:cargo/serde@1.0.0")),
            edge(&source, "serde", "^1.1", None),
            edge(&source, "serde", "^2", Some("pkg:cargo/serde@2.0.0")),
        ];
        let checked = CheckedPackageGraphFacts::new(vec![(
            source.clone(),
            DependencyFacts::Known(rows.clone().into_boxed_slice()),
        )])
        .expect("checked graph facts");
        let index = PackageGraphIndex::from_checked_facts(&checked);
        (checked, index, source, rows)
    }

    fn page_for(
        request: &PackageGraphPageRequest,
        checked: &CheckedPackageGraphFacts,
        index: &PackageGraphIndex,
    ) -> PackageGraphPage {
        expected_page(request, [0xa1; 32], checked, index).expect("expected page")
    }

    fn rewrite_more_cursor(page: &mut PackageGraphPage, edge_id: [u8; 32]) {
        let PackageGraphPageTerminal::More(cursor) = &mut page.terminal else {
            panic!("fixture page should have a continuation");
        };
        cursor.after_edge_id = edge_id;
    }

    #[test]
    fn checked_facts_authenticate_forward_page_membership_state_and_source() {
        let (checked, index, source, _) = checked_graph();
        let request = PackageGraphPageRequest::new(
            source.coordinate.clone(),
            PackageGraphDirection::Dependencies,
            None,
            1,
        )
        .expect("request");
        let page = page_for(&request, &checked, &index);
        assert_eq!(
            page.admit_against_checked_facts(&request, [0xa1; 32], &checked, &index),
            Ok(())
        );
        assert!(matches!(page.terminal, PackageGraphPageTerminal::More(_)));

        let mut deleted = page.clone();
        deleted.rows = Box::new([]);
        deleted.terminal = PackageGraphPageTerminal::Complete;
        assert_eq!(
            deleted.admit_against_checked_facts(&request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::FactsMismatch)
        );

        let inserted = edge(&source, "inserted", "*", None);
        let mut forged_insertion = page.clone();
        forged_insertion.rows = vec![inserted.clone()].into_boxed_slice();
        rewrite_more_cursor(&mut forged_insertion, inserted.facts_version);
        assert_eq!(
            forged_insertion.admit_against_checked_facts(&request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::FactsMismatch)
        );

        let mut forged_state = page.clone();
        forged_state.rows = Box::new([]);
        forged_state.knowledge = PackageGraphKnowledge::Unknown {
            reason: Some(product_text("changed state")),
        };
        forged_state.terminal = PackageGraphPageTerminal::Complete;
        assert_eq!(
            forged_state.admit_against_checked_facts(&request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::FactsMismatch)
        );

        let mut missing_source = page;
        missing_source.source = None;
        missing_source.rows = Box::new([]);
        missing_source.knowledge = PackageGraphKnowledge::Unknown { reason: None };
        missing_source.terminal = PackageGraphPageTerminal::Complete;
        assert_eq!(
            missing_source.admit_against_checked_facts(&request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::FactsMismatch)
        );
    }

    #[test]
    fn checked_facts_authenticate_reverse_pages_and_keyset_continuations() {
        let (checked, index, _, rows) = checked_graph();
        let target = PackageReference::parse("pkg:cargo/serde@1.0.0").expect("target package");
        let first_request = PackageGraphPageRequest::new(
            target.clone(),
            PackageGraphDirection::Dependents,
            None,
            1,
        )
        .expect("request");
        let first = page_for(&first_request, &checked, &index);
        assert_eq!(
            first.admit_against_checked_facts(&first_request, [0xa1; 32], &checked, &index),
            Ok(())
        );
        let PackageGraphPageTerminal::More(cursor) = &first.terminal else {
            panic!("reverse fixture should have another page");
        };

        let second_request = first_request.clone().with_cursor(cursor.clone());
        let second = page_for(&second_request, &checked, &index);
        assert_eq!(
            second.admit_against_checked_facts(&second_request, [0xa1; 32], &checked, &index),
            Ok(())
        );
        assert_eq!(second.rows.len(), 1);

        let mut deleted = first.clone();
        deleted.rows = Box::new([]);
        deleted.terminal = PackageGraphPageTerminal::Complete;
        assert_eq!(
            deleted.admit_against_checked_facts(&first_request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::FactsMismatch)
        );

        let matching = rows
            .iter()
            .find(|row| {
                row.target.name.as_str() == "serde"
                    && row
                        .target
                        .resolved
                        .as_ref()
                        .is_none_or(|resolved| resolved == &target)
                    && first.rows[0].facts_version != row.facts_version
            })
            .expect("another matching edge");
        let mut forged_insertion = first.clone();
        forged_insertion.rows = vec![matching.clone()].into_boxed_slice();
        rewrite_more_cursor(&mut forged_insertion, matching.facts_version);
        assert_eq!(
            forged_insertion.admit_against_checked_facts(
                &first_request,
                [0xa1; 32],
                &checked,
                &index
            ),
            Err(PackageGraphPageError::FactsMismatch)
        );

        let mut forged_source = first;
        forged_source.source = Some(source());
        assert_eq!(
            forged_source.admit_against_checked_facts(&first_request, [0xa1; 32], &checked, &index),
            Err(PackageGraphPageError::PageShape)
        );
    }

    #[test]
    fn authority_selector_round_trips_typed_source_identities() {
        let authorities = [
            PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
                [0x31; 32],
            )),
            PackageGraphSourceAuthority::Forge([0x42; 32]),
            PackageGraphSourceAuthority::Archive([0x53; 32]),
            PackageGraphSourceAuthority::Local([0x64; 32]),
            PackageGraphSourceAuthority::Unattributed,
        ];

        for authority in authorities {
            let selector = authority.selector();
            assert_eq!(
                PackageGraphSourceAuthority::parse_selector(&selector),
                Ok(authority)
            );
        }
        assert_eq!(
            PackageGraphSourceAuthority::parse_selector("registry:xyz"),
            Err(PackageGraphPageError::AuthoritySelector)
        );
    }

    #[test]
    fn request_admission_bounds_pages_and_direction_specific_authority() {
        let authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x71; 32]),
        );
        assert_eq!(
            PackageGraphPageRequest::new(package(), PackageGraphDirection::Dependencies, None, 0,)
                .err(),
            Some(PackageGraphPageError::PageBound)
        );
        assert_eq!(
            PackageGraphPageRequest::new(
                package(),
                PackageGraphDirection::Dependents,
                Some(authority),
                1,
            )
            .err(),
            Some(PackageGraphPageError::AuthorityNotApplicable)
        );
        assert!(
            PackageGraphPageRequest::new(
                package(),
                PackageGraphDirection::Dependencies,
                None,
                MAX_PACKAGE_GRAPH_PAGE_ROWS,
            )
            .is_ok()
        );
    }

    #[test]
    fn cursor_recipe_binds_coordinate_direction_authority_and_limit() {
        let authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x22; 32]),
        );
        let source = PackageGraphSourceKey::new(package(), authority);
        let base =
            PackageGraphPageRequest::new(package(), PackageGraphDirection::Dependencies, None, 32)
                .expect("valid request");
        let recipe = base.recipe(Some(&source));
        let other_limit =
            PackageGraphPageRequest::new(package(), PackageGraphDirection::Dependencies, None, 31)
                .expect("valid request");
        let reverse =
            PackageGraphPageRequest::new(package(), PackageGraphDirection::Dependents, None, 32)
                .expect("valid request");
        assert_ne!(recipe, other_limit.recipe(Some(&source)));
        assert_ne!(recipe, reverse.recipe(Some(&source)));
        let other_source = PackageGraphSourceKey::new(
            package(),
            PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
                [0x23; 32],
            )),
        );
        assert_ne!(recipe, base.recipe(Some(&other_source)));
    }

    #[test]
    fn known_empty_remains_distinct_from_unknown_and_unavailable() {
        let authority = PackageGraphSourceAuthority::Unattributed;
        let source = PackageGraphSourceKey::new(package(), authority);
        let page = PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [1; 32],
            facts_witness: [2; 32],
            catalog_snapshot: None,
            package: package(),
            direction: PackageGraphDirection::Dependencies,
            source: Some(source),
            knowledge: PackageGraphKnowledge::Known,
            rows: Box::new([]),
            terminal: PackageGraphPageTerminal::Complete,
        };
        assert_eq!(page.admit(), Ok(()));
        assert_ne!(
            page.knowledge,
            PackageGraphKnowledge::Unknown { reason: None }
        );
    }

    #[test]
    fn page_rejects_mutated_edge_payload_under_original_identity() {
        let coordinate = package();
        let authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x31; 32]),
        );
        let source = PackageGraphSourceKey::new(coordinate.clone(), authority);
        let mut edge = PackageDependencyRecord::new_with_source_authority(
            coordinate.clone(),
            authority,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
                .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [0x41; 32],
                provenance: [0x42; 32],
            },
        );
        edge.target = PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^9", None)
            .expect("mutated target");
        let page = PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [1; 32],
            facts_witness: [2; 32],
            catalog_snapshot: None,
            package: coordinate,
            direction: PackageGraphDirection::Dependencies,
            source: Some(source),
            knowledge: PackageGraphKnowledge::Known,
            rows: vec![edge].into_boxed_slice(),
            terminal: PackageGraphPageTerminal::Complete,
        };
        assert_eq!(page.admit(), Err(PackageGraphPageError::PageShape));
    }

    #[test]
    fn catalog_snapshot_and_query_recipe_are_admitted_across_pages() {
        let package = package();
        let authority = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x41; 32]),
        );
        let source = PackageGraphSourceKey::new(package.clone(), authority);
        let request = PackageGraphPageRequest::new(
            package.clone(),
            PackageGraphDirection::Dependencies,
            None,
            1,
        )
        .expect("request")
        .bind_catalog_snapshot([0x52; 32])
        .expect("catalog binding");
        let edge = PackageDependencyRecord::new_with_source_authority(
            package.clone(),
            authority,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
                .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [0x61; 32],
                provenance: [0x62; 32],
            },
        );
        let cursor = PackageGraphCursor {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [0x11; 32],
            facts_witness: [0x12; 32],
            recipe: request.recipe(Some(&source)),
            catalog_snapshot: request.catalog_snapshot,
            source: Some(source.clone()),
            after_edge_id: edge.facts_version,
        };
        let page = PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: cursor.view_root,
            facts_witness: cursor.facts_witness,
            catalog_snapshot: cursor.catalog_snapshot,
            package: package.clone(),
            direction: PackageGraphDirection::Dependencies,
            source: Some(source),
            knowledge: PackageGraphKnowledge::Known,
            rows: vec![edge].into_boxed_slice(),
            terminal: PackageGraphPageTerminal::More(cursor.clone()),
        };
        assert_eq!(page.admit_for(&request), Ok(()));

        let continued = request.clone().with_cursor(cursor.clone());
        assert_eq!(continued.catalog_snapshot, Some([0x52; 32]));
        let wrong_limit = PackageGraphPageRequest::new(
            package.clone(),
            PackageGraphDirection::Dependencies,
            None,
            2,
        )
        .expect("request")
        .with_cursor(cursor.clone());
        assert_eq!(
            page.admit_for(&wrong_limit),
            Err(PackageGraphPageError::CursorMismatch)
        );
        assert_eq!(
            request
                .clone()
                .with_cursor(cursor)
                .bind_catalog_snapshot([0x53; 32]),
            Err(PackageGraphPageError::StaleCursor)
        );
    }

    #[test]
    fn unknown_and_unavailable_pages_cannot_smuggle_rows() {
        let authority = PackageGraphSourceAuthority::Unattributed;
        let source = PackageGraphSourceKey::new(package(), authority);
        let edge = PackageDependencyRecord::new_with_source_authority(
            package(),
            authority,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
                .expect("target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [0x71; 32],
                provenance: [0x72; 32],
            },
        );
        let mut page = PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [1; 32],
            facts_witness: [2; 32],
            catalog_snapshot: None,
            package: package(),
            direction: PackageGraphDirection::Dependencies,
            source: Some(source),
            knowledge: PackageGraphKnowledge::Unknown { reason: None },
            rows: vec![edge].into_boxed_slice(),
            terminal: PackageGraphPageTerminal::Complete,
        };
        assert_eq!(page.admit(), Err(PackageGraphPageError::PageShape));
        page.knowledge = PackageGraphKnowledge::Unavailable {
            reason: ProductText::new("source did not answer").expect("reason"),
        };
        assert_eq!(page.admit(), Err(PackageGraphPageError::PageShape));
    }
}
