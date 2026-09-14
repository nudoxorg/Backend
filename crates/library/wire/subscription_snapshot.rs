//! One typed wire projection for bounded local subscriptions.
//!
//! The local control envelope is transport specific, but the successful
//! payload inside it is deliberately owned here.  CLI, MCP, desktop, and the
//! local daemon therefore share one decoder and one cursor/event admission
//! state machine instead of maintaining subtly different JSON parsers.

use super::subscription::{cursor_from_wire_against, reset_reason, reset_reason_name, same_stream};
use super::{
    BasisWire, CoverageWire, CursorWire, DTO_VERSION, RowIdWire, RowWire, WireCertificate,
    WireClaim, basis_from_wire, basis_from_wire_with_capability, basis_object, basis_to_wire,
    coverage_from_wire, coverage_to_wire, cursor_to_wire, ensure_version, row_from_wire,
    row_from_wire_against_with_capability, row_id_from_wire, row_id_to_wire,
};
use crate::{
    Cursor, CursorResetReason, Frontier, MAX_SNAPSHOT_PAGE_ROWS, ViewPageCursor, ViewPageError,
    ViewRootDescriptor, ViewRootDescriptorClaim, ViewSnapshotPage, canonical::encode_id,
};
use serde::{Deserialize, Serialize};

use super::reply_admission::{CapabilityAdmission, CoverageAdmission, VerifierAdmission};
use backend_version::ProducerObservationVerifier;

mod hydration;

pub use hydration::SnapshotHydrator;

/// A bounded reset page carrying a constant-size root descriptor.
///
/// This DTO is intentionally separate from [`crate::SubscriptionDto::Reset`]. The
/// legacy reset remains useful for tiny compatibility replies, while this
/// form lets a daemon send O(1) reset metadata followed by O(page) payloads.
/// The producer certificate is attached to each page and covers its rows;
/// the final hydrated row set is admitted against the descriptor's root and
/// coverage before becoming a [`ViewRoot`](crate::ViewRoot).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPageDto {
    cursor: Cursor,
    descriptor: ViewRootDescriptor,
    page: ViewSnapshotPage,
    reason: CursorResetReason,
    certificate: Option<WireCertificate>,
}

/// A bounded page admitted from a producer certificate while its visible root
/// remains deferred.
///
/// A reset often targets a root that the client has never seen.  Requiring an
/// already typed [`ViewRootDescriptor`] in that case would force a full query
/// before hydration and reintroduce the O(view) path this DTO is designed to
/// remove.  This claim therefore carries typed recipe/version/source values,
/// but keeps the visible root as a fixed-width commitment until
/// [`SnapshotHydrator::finish`] recomputes it from all pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPageClaim {
    cursor_sequence: u64,
    descriptor: ViewRootDescriptorClaim,
    after: Option<crate::RowId>,
    rows: Box<[crate::Row]>,
    next_after: Option<crate::RowId>,
    reason: CursorResetReason,
    certificate: WireCertificate,
}

impl SnapshotPageClaim {
    /// Decodes and admits one page without requiring the target visible root
    /// to have been seen before.
    ///
    /// Recipe/version/source identities and every row identity are checked
    /// immediately from their producer preimages.  The visible root remains
    /// a fixed-width commitment and is promoted only by [`SnapshotHydrator`]
    /// after the complete row set has been received.  `expected_source_root`
    /// lets a client pin reset pages to the source authority it already
    /// trusts while still allowing the derived view root to change.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn decode(
        bytes: &[u8],
        previous: Cursor,
        expected_source_root: Option<crate::ViewStateRoot>,
        capability: Option<crate::CoverageCapability>,
    ) -> Result<Self, String> {
        Self::decode_with_admission(
            bytes,
            previous,
            expected_source_root,
            &CapabilityAdmission(capability),
        )
    }

    /// Decodes and admits one page using an authenticated producer boundary.
    ///
    /// This is the process-boundary counterpart to [`Self::decode`]. It turns
    /// the certificate's bounded producer observation into the opaque coverage
    /// capability retained by the deferred descriptor, without making that
    /// capability serializable or reconstructible from wire fields alone.
    /// # Errors
    ///
    /// Returns an error when the payload is malformed or `verifier` rejects
    /// the producer observation carried by its certificate.
    pub fn decode_with_verifier<V: ProducerObservationVerifier>(
        bytes: &[u8],
        previous: Cursor,
        expected_source_root: Option<crate::ViewStateRoot>,
        verifier: &V,
    ) -> Result<Self, String> {
        Self::decode_with_admission(
            bytes,
            previous,
            expected_source_root,
            &VerifierAdmission(verifier),
        )
    }

    fn decode_with_admission(
        bytes: &[u8],
        previous: Cursor,
        expected_source_root: Option<crate::ViewStateRoot>,
        admission: &impl CoverageAdmission,
    ) -> Result<Self, String> {
        let wire: SnapshotPageWire =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(wire.version, "snapshot page")?;
        if wire.kind != "reset_page" {
            return Err("snapshot page kind mismatch".to_owned());
        }
        let certificate = wire.certificate.as_ref().ok_or_else(|| {
            "identity-bearing snapshot page requires a producer certificate".to_owned()
        })?;
        let recipe = certificate.key_bytes::<crate::canonical::ViewRecipeSchema>(
            super::WireSchema::ViewRecipe,
            &wire.root.recipe,
        )?;
        let version = certificate.version_value::<crate::canonical::ViewVersionSchema>(
            super::WireSchema::ViewVersion,
            &wire.root.version,
        )?;
        let root = certificate.root_commitment_bytes::<crate::ViewRelation>(
            super::WireSchema::ViewRelation,
            &wire.root.root,
        )?;
        let coverage = wire
            .root
            .coverage
            .iter()
            .cloned()
            .map(coverage_from_wire)
            .collect::<Result<Vec<_>, _>>()?;
        if coverage.is_empty() {
            return Err("snapshot page descriptor omits producer coverage".to_owned());
        }
        let Some(capability) = admission.admit(certificate, basis_object(&wire.root.basis))? else {
            return Err(
                "snapshot page requires an externally admitted coverage capability".to_owned(),
            );
        };
        let basis = basis_from_wire_with_capability(&wire.root.basis, certificate, &capability)?;
        if expected_source_root.is_some_and(|expected| expected != basis.root) {
            return Err("snapshot page source root does not match the admitted source".to_owned());
        }
        let frontier = super::frontier_from_wire_with_capability(
            &wire.root.frontier,
            certificate,
            &capability,
        )?;
        let bootstrap = previous == Cursor::new();
        if !bootstrap
            && (previous.recipe() != recipe
                || previous.branch() != frontier.branch
                || previous.log() != frontier.log
                || previous.schema() != frontier.schema)
        {
            return Err("snapshot page cursor stream mismatch".to_owned());
        }
        let descriptor = ViewRootDescriptorClaim::from_parts(crate::view::DescriptorParts {
            recipe,
            version,
            root,
            basis,
            frontier,
            coverage: coverage.into_boxed_slice(),
            capability: Some(capability.clone()),
            row_count: wire.root.row_count,
        });
        admit_raw_cursor_claim(certificate, &wire.cursor, &descriptor)?;
        let cursor_sequence = wire.cursor.sequence();
        // `Cursor::new()` is both the absence of a prior observation and the
        // valid cursor of an empty genesis view. Bootstrap must therefore
        // admit sequence zero; every resumed stream still requires strict
        // forward progress.
        if !bootstrap && cursor_sequence <= previous.sequence() {
            return Err("snapshot page cursor is not newer than the prior cursor".to_owned());
        }
        let after = page_claim_after(&wire.page.cursor, &descriptor, certificate, &capability)?;
        let (rows, next_after) =
            decode_page_rows(&wire.page, &descriptor, certificate, &capability, after)?;
        Ok(Self {
            cursor_sequence,
            descriptor,
            after,
            rows: rows.into_boxed_slice(),
            next_after,
            reason: reset_reason(&wire.reason)?,
            certificate: wire.certificate.ok_or_else(|| {
                "snapshot page certificate disappeared during admission".to_owned()
            })?,
        })
    }

    /// Returns the deferred descriptor admitted from the producer certificate.
    #[must_use]
    pub const fn descriptor(&self) -> &ViewRootDescriptorClaim {
        &self.descriptor
    }

    /// Returns the exact owner sequence paired with the reset descriptor.
    #[must_use]
    pub const fn cursor_sequence(&self) -> u64 {
        self.cursor_sequence
    }

    /// Returns the row anchor used for this page, if any.
    #[must_use]
    pub const fn after(&self) -> Option<crate::RowId> {
        self.after
    }

    /// Returns the bounded authenticated rows in this page.
    #[must_use]
    pub fn rows(&self) -> &[crate::Row] {
        &self.rows
    }

    /// Returns the next row anchor, if another page is required.
    #[must_use]
    pub const fn next_after(&self) -> Option<crate::RowId> {
        self.next_after
    }

    /// Returns the exact opaque continuation token for the following page.
    ///
    /// The token is derived from the deferred descriptor bytes and final row
    /// identity, so a client can compare it with the transport envelope
    /// before retaining a page continuation.  No deferred root is promoted by
    /// this operation.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn next_token(&self) -> Result<Option<Box<[u8]>>, String> {
        self.next_after
            .map(|after| {
                serde_json::to_vec(&PageCursorWire {
                    recipe: encode_id(self.descriptor.recipe().as_bytes()),
                    version: encode_id(self.descriptor.version().as_bytes()),
                    root: encode_id(self.descriptor.root_bytes()),
                    after: Some(row_id_to_wire(after)),
                })
                .map(Vec::into_boxed_slice)
                .map_err(|error| error.to_string())
            })
            .transpose()
    }

    /// Returns why the producer requested a reset.
    #[must_use]
    pub const fn reason(&self) -> CursorResetReason {
        self.reason
    }

    /// Returns the producer certificate retained for this bounded page.
    #[must_use]
    pub const fn certificate(&self) -> &WireCertificate {
        &self.certificate
    }
}
fn admit_raw_cursor_claim(
    certificate: &WireCertificate,
    cursor: &CursorWire,
    descriptor: &ViewRootDescriptorClaim,
) -> Result<(), String> {
    let recipe = encode_id(descriptor.recipe().as_bytes());
    let version = encode_id(descriptor.version().as_bytes());
    let branch = encode_id(descriptor.frontier().branch.as_bytes());
    let log = encode_id(descriptor.frontier().log.as_bytes());
    let schema = descriptor.frontier().schema;
    let root = encode_id(descriptor.root_bytes());
    let mut found = false;
    for claim in &certificate.claims {
        let WireClaim::Cursor {
            recipe: claimed_recipe,
            version: claimed_version,
            branch: claimed_branch,
            log: claimed_log,
            schema: claimed_schema,
            root: claimed_root,
            sequence,
        } = claim
        else {
            continue;
        };
        if found
            || claimed_recipe != &recipe
            || claimed_version != &version
            || claimed_branch != &branch
            || claimed_log != &log
            || *claimed_schema != schema
            || claimed_root != &root
            || *sequence != cursor.sequence()
        {
            return Err("snapshot page cursor certificate does not match".to_owned());
        }
        found = true;
    }
    if !found {
        return Err("missing snapshot page cursor certificate".to_owned());
    }
    if cursor.recipe() != recipe
        || cursor.version() != version
        || cursor.branch() != branch
        || cursor.log() != log
        || cursor.schema() != schema
        || cursor.root() != root
        || cursor.query_offset() != 0
    {
        return Err("snapshot page cursor does not match its descriptor".to_owned());
    }
    Ok(())
}

fn decode_page_rows(
    page: &SnapshotPageBodyWire,
    descriptor: &ViewRootDescriptorClaim,
    certificate: &WireCertificate,
    capability: &crate::CoverageCapability,
    after: Option<crate::RowId>,
) -> Result<(Vec<crate::Row>, Option<crate::RowId>), String> {
    let rows = page
        .rows
        .iter()
        .cloned()
        .map(|row| {
            row_from_wire_against_with_capability(row, descriptor.basis(), certificate, capability)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if page.total_rows != descriptor.row_count() || rows.len() > MAX_SNAPSHOT_PAGE_ROWS {
        return Err("snapshot page row count exceeds its descriptor".to_owned());
    }
    if after.is_some_and(|after| rows.first().is_some_and(|row| row.id <= after)) {
        return Err("snapshot page starts before its continuation anchor".to_owned());
    }
    if rows.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err("snapshot page rows are not strictly ordered".to_owned());
    }
    let next_after = page
        .next
        .as_ref()
        .map(|next| page_claim_after(next, descriptor, certificate, capability))
        .transpose()?
        .flatten();
    if next_after.is_some() && rows.is_empty() {
        return Err("snapshot continuation cannot follow an empty page".to_owned());
    }
    if next_after.is_some() && next_after != rows.last().map(|row| row.id) {
        return Err("snapshot continuation does not follow the final row".to_owned());
    }
    Ok((rows, next_after))
}

fn page_claim_after(
    cursor: &PageCursorWire,
    descriptor: &ViewRootDescriptorClaim,
    certificate: &WireCertificate,
    capability: &crate::CoverageCapability,
) -> Result<Option<crate::RowId>, String> {
    if cursor.recipe != encode_id(descriptor.recipe().as_bytes())
        || cursor.version != encode_id(descriptor.version().as_bytes())
        || cursor.root != encode_id(descriptor.root_bytes())
    {
        return Err("snapshot page cursor root mismatch".to_owned());
    }
    cursor
        .after
        .as_ref()
        .map(|row| match row.kind.as_str() {
            "package" => certificate
                .key_value::<crate::canonical::PackageSchema>(super::WireSchema::Package, &row.id)
                .or_else(|_| {
                    certificate.producer_key_value(super::WireSchema::Package, &row.id, capability)
                })
                .map(crate::RowId::Package),
            "symbol" => certificate
                .key_value::<crate::canonical::SymbolSchema>(super::WireSchema::Symbol, &row.id)
                .or_else(|_| {
                    certificate.producer_key_value(super::WireSchema::Symbol, &row.id, capability)
                })
                .map(crate::RowId::Symbol),
            "object" => row_id_from_wire(row, certificate),
            _ => Err("unknown stable row identity kind".to_owned()),
        })
        .transpose()
}

impl SnapshotPageDto {
    /// Constructs a producer-owned first page without inventing a predecessor
    /// cursor.  The owner already controls the exact replacement cursor; the
    /// receiving hydrator performs the monotone comparison with its stale
    /// cursor when the page is admitted.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn from_owner(
        cursor: Cursor,
        descriptor: ViewRootDescriptor,
        page: ViewSnapshotPage,
        reason: CursorResetReason,
    ) -> Result<Self, String> {
        if cursor.recipe() != descriptor.recipe()
            || cursor.version() != descriptor.version()
            || cursor.root() != descriptor.root()
        {
            return Err("snapshot reset cursor does not match its root descriptor".to_owned());
        }
        if descriptor.capability().is_none() {
            return Err(
                "snapshot reset descriptor lacks a producer coverage capability".to_owned(),
            );
        }
        admit_page_against_descriptor(&descriptor, &page)?;
        Ok(Self {
            cursor,
            descriptor,
            page,
            reason,
            certificate: None,
        })
    }

    /// Constructs one reset page after checking stream, descriptor, and page
    /// identity bindings.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn try_new(
        previous: Cursor,
        cursor: Cursor,
        descriptor: ViewRootDescriptor,
        page: ViewSnapshotPage,
        reason: CursorResetReason,
    ) -> Result<Self, String> {
        if cursor.sequence() <= previous.sequence() {
            return Err("snapshot reset cursor is not newer than the prior cursor".to_owned());
        }
        if !same_stream(previous, cursor) {
            return Err("snapshot reset cursor changed its stream identity".to_owned());
        }
        Self::from_owner(cursor, descriptor, page, reason)
    }

    /// Attaches the producer certificate for this page.
    #[must_use]
    pub fn with_certificate(mut self, certificate: WireCertificate) -> Self {
        self.certificate = Some(certificate);
        self
    }

    /// Returns the exact owner cursor associated with this reset.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Returns the constant-size checked root descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &ViewRootDescriptor {
        &self.descriptor
    }

    /// Returns the bounded page payload.
    #[must_use]
    pub const fn page(&self) -> &ViewSnapshotPage {
        &self.page
    }

    /// Returns the explicit stale-cursor reset reason.
    #[must_use]
    pub const fn reason(&self) -> CursorResetReason {
        self.reason
    }

    /// Returns the certificate carried by this page.
    #[must_use]
    pub const fn certificate(&self) -> Option<&WireCertificate> {
        self.certificate.as_ref()
    }

    /// Returns the opaque continuation token for the next page.
    ///
    /// The token contains only the fixed-size typed page cursor.  It is safe
    /// to persist and echo verbatim; the producer still checks it against the
    /// lease's descriptor and rejects a replayed or out-of-order continuation.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn next_token(&self) -> Result<Option<Box<[u8]>>, String> {
        self.page
            .next()
            .map(|cursor| {
                serde_json::to_vec(&page_cursor_to_wire(cursor))
                    .map(Vec::into_boxed_slice)
                    .map_err(|error| error.to_string())
            })
            .transpose()
    }

    /// Decodes one page against a caller-owned descriptor.
    ///
    /// Root and recipe/version identities are compared with the expected
    /// typed descriptor, while row identities and source basis fields are
    /// admitted from the bounded producer certificate. No visible-root
    /// canonical node is required on the wire: after the final page,
    /// [`ViewRootDescriptor::admit_rows`] recomputes and checks that root.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong schema, cursor, producer, row order, page
    /// continuation, or certificate claims.
    pub fn decode_against(
        bytes: &[u8],
        previous: Cursor,
        expected: &ViewRootDescriptor,
    ) -> Result<Self, String> {
        let wire: SnapshotPageWire =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(wire.version, "snapshot page")?;
        if wire.kind != "reset_page" {
            return Err("snapshot page kind mismatch".to_owned());
        }
        let certificate = wire.certificate.as_ref().ok_or_else(|| {
            "identity-bearing snapshot page requires a producer certificate".to_owned()
        })?;
        admit_descriptor_wire(&wire.root, expected, certificate)?;
        let sequence = wire.cursor.sequence();
        let expected_cursor = Cursor::for_view(
            expected.recipe(),
            expected.version(),
            Frontier::new(
                expected.frontier().branch,
                expected.frontier().log,
                expected.frontier().schema,
                expected.root(),
                sequence,
            ),
        );
        let cursor = cursor_from_wire_against(&wire.cursor, expected_cursor)?;
        certificate.cursor_claim(cursor)?;
        let page_cursor = page_cursor_from_wire(&wire.page.cursor, expected, certificate)?;
        let rows = wire
            .page
            .rows
            .into_iter()
            .map(|row| row_from_wire(row, certificate))
            .collect::<Result<Vec<_>, _>>()?;
        let next = wire
            .page
            .next
            .as_ref()
            .map(|next| page_cursor_from_wire(next, expected, certificate))
            .transpose()?;
        let page = ViewSnapshotPage::from_parts(page_cursor, rows, next, wire.page.total_rows)
            .map_err(snapshot_page_error)?;
        admit_page_against_descriptor(expected, &page)?;
        let reason = reset_reason(&wire.reason)?;
        let certificate = wire
            .certificate
            .ok_or_else(|| "snapshot page certificate disappeared during admission".to_owned())?;
        Self::try_new(previous, cursor, expected.clone(), page, reason)
            .map(|page| page.with_certificate(certificate))
    }
}

/// Encodes the constant-size identity descriptor of one checked view root.
///
/// Journal and lease implementations use this canonical representation to
/// validate a target after applying a compact delta without serializing the
/// complete visible relation.
/// # Errors
///
/// Returns an error when the encoded identity or checked state is invalid.
pub fn encode_view_root_descriptor(descriptor: &ViewRootDescriptor) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&descriptor_to_wire(descriptor)).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPageWire {
    version: u16,
    kind: String,
    cursor: CursorWire,
    root: SnapshotDescriptorWire,
    page: SnapshotPageBodyWire,
    reason: String,
    certificate: Option<WireCertificate>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPageOut<'a> {
    version: u16,
    kind: &'static str,
    cursor: CursorWire,
    root: SnapshotDescriptorWire,
    page: SnapshotPageBodyWire,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<&'a WireCertificate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDescriptorWire {
    recipe: String,
    version: String,
    root: String,
    basis: BasisWire,
    frontier: super::FrontierWire,
    coverage: Vec<CoverageWire>,
    row_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPageBodyWire {
    cursor: PageCursorWire,
    rows: Vec<RowWire>,
    next: Option<PageCursorWire>,
    total_rows: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageCursorWire {
    recipe: String,
    version: String,
    root: String,
    after: Option<RowIdWire>,
}

impl Serialize for SnapshotPageDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SnapshotPageOut {
            version: DTO_VERSION,
            kind: "reset_page",
            cursor: cursor_to_wire(self.cursor),
            root: descriptor_to_wire(&self.descriptor),
            page: page_to_wire(&self.page),
            reason: reset_reason_name(self.reason),
            certificate: self.certificate.as_ref(),
        }
        .serialize(serializer)
    }
}

fn descriptor_to_wire(descriptor: &ViewRootDescriptor) -> SnapshotDescriptorWire {
    SnapshotDescriptorWire {
        recipe: encode_id(descriptor.recipe().as_bytes()),
        version: encode_id(descriptor.version().as_bytes()),
        root: encode_id(descriptor.root().as_bytes()),
        basis: basis_to_wire(descriptor.basis()),
        frontier: super::frontier_to_wire(descriptor.frontier()),
        coverage: descriptor
            .coverage()
            .iter()
            .copied()
            .map(coverage_to_wire)
            .collect(),
        row_count: descriptor.row_count(),
    }
}

fn page_to_wire(page: &ViewSnapshotPage) -> SnapshotPageBodyWire {
    SnapshotPageBodyWire {
        cursor: page_cursor_to_wire(page.cursor()),
        rows: page.rows().iter().map(super::row_to_wire).collect(),
        next: page.next().map(page_cursor_to_wire),
        total_rows: page.total_rows(),
    }
}

fn page_cursor_to_wire(cursor: ViewPageCursor) -> PageCursorWire {
    PageCursorWire {
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        root: encode_id(cursor.root().as_bytes()),
        after: cursor.after().map(row_id_to_wire),
    }
}

fn admit_descriptor_wire(
    wire: &SnapshotDescriptorWire,
    expected: &ViewRootDescriptor,
    certificate: &WireCertificate,
) -> Result<(), String> {
    if wire.recipe != encode_id(expected.recipe().as_bytes())
        || wire.version != encode_id(expected.version().as_bytes())
        || wire.root != encode_id(expected.root().as_bytes())
        || wire.row_count != expected.row_count()
    {
        return Err("snapshot root descriptor identity mismatch".to_owned());
    }
    certificate.key_bytes::<crate::canonical::ViewRecipeSchema>(
        super::WireSchema::ViewRecipe,
        &wire.recipe,
    )?;
    certificate.version_value::<crate::canonical::ViewVersionSchema>(
        super::WireSchema::ViewVersion,
        &wire.version,
    )?;
    certificate.root_commitment(super::WireSchema::ViewRelation, &wire.root)?;
    let basis = basis_from_wire(&wire.basis, certificate)?;
    if basis != expected.basis()
        || super::frontier_from_wire(&wire.frontier, certificate)? != expected.frontier()
    {
        return Err("snapshot root descriptor basis mismatch".to_owned());
    }
    let coverage = wire
        .coverage
        .iter()
        .cloned()
        .map(coverage_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    if coverage.as_slice() != expected.coverage() {
        return Err("snapshot root descriptor coverage mismatch".to_owned());
    }
    let Some(capability) = expected.capability() else {
        return Err("snapshot descriptor lacks an admitted coverage capability".to_owned());
    };
    certificate.admit_coverage_capability(basis_object(&wire.basis), &capability)?;
    Ok(())
}

fn page_cursor_from_wire(
    wire: &PageCursorWire,
    expected: &ViewRootDescriptor,
    certificate: &WireCertificate,
) -> Result<ViewPageCursor, String> {
    if wire.recipe != encode_id(expected.recipe().as_bytes())
        || wire.version != encode_id(expected.version().as_bytes())
        || wire.root != encode_id(expected.root().as_bytes())
    {
        return Err("snapshot page cursor root mismatch".to_owned());
    }
    let after = wire
        .after
        .as_ref()
        .map(|row| row_id_from_wire(row, certificate))
        .transpose()?;
    Ok(ViewPageCursor::from_parts(
        expected.recipe(),
        expected.version(),
        expected.root(),
        after,
    ))
}

fn admit_page_against_descriptor(
    descriptor: &ViewRootDescriptor,
    page: &ViewSnapshotPage,
) -> Result<(), String> {
    if page.total_rows() != descriptor.row_count()
        || page.rows().len() > MAX_SNAPSHOT_PAGE_ROWS
        || page.cursor().recipe() != descriptor.recipe()
        || page.cursor().version() != descriptor.version()
        || page.cursor().root() != descriptor.root()
        || page.next().is_some_and(|next| {
            next.recipe() != descriptor.recipe()
                || next.version() != descriptor.version()
                || next.root() != descriptor.root()
        })
    {
        return Err("snapshot page does not match its root descriptor".to_owned());
    }
    if page
        .rows()
        .iter()
        .any(|row| row.basis != descriptor.basis())
    {
        return Err("snapshot page row basis mismatch".to_owned());
    }
    Ok(())
}

fn snapshot_page_error(error: ViewPageError) -> String {
    format!("invalid snapshot page: {error:?}")
}
