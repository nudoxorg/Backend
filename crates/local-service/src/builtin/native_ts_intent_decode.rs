//! Narrow V17 wire parsing and opaque, ledger-backed retention.
//!
//! The carrier is an untrusted parsed claim, not a selected-transition
//! capability. Admission and replay must independently bind the exact
//! workspace, closure, request, project and semantic/native frontiers.
//! The sealed input is retained without a second allocation and must use the
//! same admission ledger.

use backend_semantic::vocabulary::LanguageProfile;

#[derive(Debug)]
pub(super) struct RetirementIntentWireSummary<'a> {
    pub label_bytes: usize,
    pub project_bytes: usize,
    pub semantic_rows: usize,
    pub semantic_key_bytes: usize,
    pub semantic_value_bytes: usize,
    pub evidence_rows: usize,
    pub evidence_value_bytes: usize,
    pub largest_evidence_value: usize,
    project: &'a [u8],
    owner: [u8; 32],
    operation_key: Option<[u8; 32]>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).ok_or("intent length overflow")?;
        let value = self.bytes.get(self.at..end).ok_or("truncated intent")?;
        self.at = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<usize, &'static str> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| "truncated count")?) as usize)
    }
    fn zero(&mut self) -> Result<(), &'static str> {
        if self.byte()? == 0 {
            Ok(())
        } else {
            Err("unsupported section")
        }
    }
    fn done(&self) -> Result<(), &'static str> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err("trailing intent bytes")
        }
    }
}
fn add(a: usize, b: usize) -> Result<usize, &'static str> {
    a.checked_add(b).ok_or("intent length overflow")
}

fn validate_evidence_tail(
    row: &[u8],
    path_end: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), &'static str> {
    let rest = row.get(path_end..).ok_or("truncated evidence")?;
    if rest.len() < 69 || rest[68] != 0 {
        return Err("invalid evidence payload");
    }
    let byte_length = u32::from_be_bytes(
        rest[64..68]
            .try_into()
            .map_err(|_| "invalid evidence size")?,
    );
    if byte_length > backend_engine::builtin::MAX_NATIVE_EVIDENCE_SOURCE {
        return Err("evidence source too large");
    }
    if row.starts_with(b"NSE1") {
        return if rest.len() == 69 {
            Ok(())
        } else {
            Err("invalid legacy evidence")
        };
    }
    let kind = *rest.get(69).ok_or("missing evidence kind")?;
    if kind > 1 {
        return Err("invalid evidence kind");
    }
    let count = u32::from_be_bytes(
        rest.get(70..74)
            .ok_or("missing evidence lines")?
            .try_into()
            .map_err(|_| "invalid evidence lines")?,
    ) as usize;
    let end = 74usize
        .checked_add(count.checked_mul(4).ok_or("evidence length overflow")?)
        .ok_or("evidence length overflow")?;
    let lines = rest.get(74..end).ok_or("truncated evidence lines")?;
    if row.starts_with(b"NSE2") {
        if end != rest.len() || (kind == 1 && count == 0) {
            return Err("invalid evidence lines");
        }
    } else if kind != 1 || rest.len().checked_sub(end) != Some(104) {
        return Err("invalid evidence headers");
    }
    let mut previous = None;
    for (i, chunk) in lines.chunks_exact(4).enumerate() {
        if i % 256 == 0 && cancelled() {
            return Err("cancelled");
        }
        let line = u32::from_be_bytes(chunk.try_into().map_err(|_| "invalid line")?);
        if (i == 0 && line != 0) || previous.is_some_and(|p| p >= line) || line > byte_length {
            return Err("invalid line map");
        }
        previous = Some(line);
    }
    Ok(())
}

/// Checks the narrow framing before any decoder allocation. This validates
/// bounded wire shapes, borrowed canonical coordinates and evidence rows.
/// It does not establish request identity or authenticated snapshot binding;
/// those remain mandatory later checks.
pub(super) fn retirement_intent_wire_summary<'a>(
    bytes: &'a [u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<RetirementIntentWireSummary<'a>, &'static str> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(5)? != b"BPIH\x11" || r.byte()? != 3 {
        return Err("unsupported retirement intent");
    }
    let package = r.take(32)?;
    let owner: [u8; 32] = package.try_into().map_err(|_| "invalid owner")?;
    let label_bytes = r.u32()?;
    if label_bytes > 4096 {
        return Err("invalid label");
    }
    let label = std::str::from_utf8(r.take(label_bytes)?).map_err(|_| "invalid label")?;
    if backend_library::package_key_borrowed(label)
        .map_err(|_| "invalid owner")?
        .to_bytes()
        != owner
    {
        return Err("owner does not match label");
    }
    if r.u32()? != 1 || r.take(32)? != package || r.byte()? != 1 {
        return Err("invalid project change");
    }
    let project_bytes = r.u32()?;
    let project = r.take(project_bytes)?;
    if project_bytes > backend_engine::builtin::ProductSourceRecord::ROW_VALUE_CAPACITY
        || !backend_engine::builtin::empty_source_project_wire_admitted(project)
    {
        return Err("unsupported project wire shape");
    }
    let semantic_rows = r.u32()?;
    if semantic_rows > 2 {
        return Err("too many semantic changes");
    }
    let (mut semantic_key_bytes, mut semantic_value_bytes) = (0, 0);
    let mut previous_semantic: Option<(u8, &str, &str, LanguageProfile)> = None;
    for _ in 0..semantic_rows {
        if cancelled() {
            return Err("cancelled");
        }
        let key_len = r.u32()?;
        if key_len > 2 * 4096 + 64 {
            return Err("semantic key too large");
        }
        let key = r.take(key_len)?;
        let mut semantic = Reader { bytes: key, at: 0 };
        let package_len = semantic.u32()?;
        if package_len > 4096 {
            return Err("semantic package too large");
        }
        let raw_package = std::str::from_utf8(semantic.take(package_len)?)
            .map_err(|_| "invalid semantic package")?;
        let is_purl = raw_package.starts_with("pkg:");
        let package_ref = if is_purl {
            backend_semantic::vocabulary::PackageUrl::validate_borrowed(raw_package)
                .map_err(|_| "invalid semantic package URL")?;
            raw_package
        } else {
            let trimmed = raw_package.trim();
            if trimmed.is_empty()
                || trimmed != raw_package
                || trimmed.len() > 4096
                || trimmed.as_bytes().contains(&0)
            {
                return Err("invalid semantic package");
            }
            trimmed
        };
        if backend_library::package_key_borrowed(package_ref)
            .map_err(|_| "invalid semantic package")?
            .to_bytes()
            != owner
        {
            return Err("semantic package owner mismatch");
        }
        let coordinate_len = semantic.u32()?;
        if coordinate_len > 4096 {
            return Err("semantic coordinate too large");
        }
        let coordinate = std::str::from_utf8(semantic.take(coordinate_len)?)
            .map_err(|_| "invalid semantic coordinate")?;
        let facts = backend_semantic::vocabulary::PackageUrl::validate_borrowed(coordinate)
            .map_err(|_| "invalid semantic coordinate")?;
        let profile_bytes = semantic.take(2)?;
        let profile = LanguageProfile::try_from([profile_bytes[0], profile_bytes[1]])
            .map_err(|_| "invalid semantic profile")?;
        if !matches!(profile, LanguageProfile::TypeScript(_)) || semantic.byte()? != 0 {
            return Err("unsupported semantic key");
        }
        if facts.ecosystem.language() != profile.language() {
            return Err("semantic language mismatch");
        }
        let start = facts
            .namespace
            .map_or(facts.name.start, |range| range.start) as usize;
        let end = facts.name.end as usize;
        let lineage = coordinate
            .get(start..end)
            .ok_or("invalid semantic lineage")?;
        backend_semantic::ir::PackageLineage::new(facts.ecosystem.as_str(), lineage)
            .map_err(|_| "invalid semantic lineage")?;
        let current = (u8::from(!is_purl), package_ref, coordinate, profile);
        if previous_semantic.is_some_and(|previous| previous >= current) {
            return Err("unordered or duplicate semantic key");
        }
        previous_semantic = Some(current);
        semantic.done()?;
        semantic_key_bytes = add(semantic_key_bytes, key_len)?;
        if r.byte()? != 1 {
            return Err("invalid semantic after-image");
        }
        let value_len = r.u32()?;
        if value_len > 512 {
            return Err("semantic value too large");
        }
        if r.take(value_len)? != b"PSP1\x02\x02" {
            return Err("unsupported semantic value");
        }
        semantic_value_bytes = add(semantic_value_bytes, value_len)?;
    }
    r.zero()?; // semantic selection
    let operation_key = match r.byte()? {
        0 => None,
        1 => {
            let key: [u8; 32] = r
                .take(32)?
                .try_into()
                .map_err(|_| "invalid operation key")?;
            if key.iter().all(|b| *b == 0) {
                return Err("invalid operation key");
            }
            Some(key)
        }
        _ => return Err("invalid operation key tag"),
    };
    if r.u32()? != 0 {
        return Err("capture changes unsupported");
    }
    r.zero()?; // capture basis
    if r.u32()? != 0 || r.u32()? != 0 {
        return Err("facts or frontier changes unsupported");
    }
    r.zero()?; // Stylo
    if r.byte()? != 1 {
        return Err("missing native basis");
    }
    let section_len = r.u32()?;
    let max_delta = 2usize
        .checked_mul(backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES)
        .and_then(|n| n.checked_add(2 * backend_engine::builtin::MAX_NATIVE_EVIDENCE_ROWS * 42))
        .ok_or("intent length overflow")?;
    if section_len > max_delta {
        return Err("native section too large");
    }
    let mut n = Reader {
        bytes: r.take(section_len)?,
        at: 0,
    };
    if n.take(32)?.iter().all(|b| *b == 0) {
        return Err("invalid workspace root");
    }
    if n.take(8)? == [0xff; 8] {
        return Err("invalid workspace sequence");
    }
    if n.take(32)?.iter().all(|b| *b == 0) {
        return Err("invalid closure");
    }
    match n.byte()? {
        0 => (),
        1 => {
            if n.take(32)?.iter().all(|b| *b == 0) {
                return Err("invalid evidence root");
            }
        }
        _ => return Err("invalid evidence root tag"),
    }
    let evidence_rows = n.u32()?;
    if evidence_rows == 0
        || evidence_rows > 2 * backend_engine::builtin::MAX_NATIVE_EVIDENCE_ROWS
        || evidence_rows > n.bytes.len().saturating_sub(n.at) / 38
    {
        return Err("invalid evidence count");
    }
    let (mut evidence_value_bytes, mut largest_evidence_value) = (0, 0);
    let mut previous: Option<&[u8]> = None;
    for _ in 0..evidence_rows {
        if cancelled() {
            return Err("cancelled");
        }
        let key = n.take(32)?;
        if previous.is_some_and(|p| p >= key) {
            return Err("unordered evidence keys");
        }
        previous = Some(key);
        if n.byte()? != 1 {
            return Err("missing predecessor evidence");
        }
        let length = n.u32()?;
        if length > backend_engine::builtin::MAX_NATIVE_EVIDENCE_STORED_VALUE {
            return Err("evidence row too large");
        }
        let row = n.take(length)?;
        if row.len() < 40
            || !matches!(row.get(..4), Some(b"NSE1" | b"NSE2" | b"NSE3"))
            || row.get(4..36) != Some(package)
        {
            return Err("invalid evidence owner or magic");
        }
        let profile = LanguageProfile::try_from([row[36], row[37]])
            .map_err(|_| "invalid evidence profile")?;
        if !matches!(profile, LanguageProfile::TypeScript(_)) {
            return Err("unsupported evidence profile");
        }
        let path_len = u16::from_be_bytes([row[38], row[39]]) as usize;
        let path_end = 40usize
            .checked_add(path_len)
            .ok_or("evidence length overflow")?;
        let path = std::str::from_utf8(row.get(40..path_end).ok_or("truncated evidence path")?)
            .map_err(|_| "invalid evidence path")?;
        let owner: [u8; 32] = package.try_into().map_err(|_| "invalid evidence owner")?;
        if backend_engine::builtin::native_source_evidence_key(owner, profile, path)
            .map_err(|_| "invalid evidence key")?
            .as_slice()
            != key
        {
            return Err("evidence key mismatch");
        }
        validate_evidence_tail(row, path_end, cancelled)?;
        evidence_value_bytes = add(evidence_value_bytes, length)?;
        largest_evidence_value = largest_evidence_value.max(length);
        n.zero()?; // after evidence is forbidden
    }
    n.done()?;
    r.zero()?; // SQL extension
    r.done()?;
    Ok(RetirementIntentWireSummary {
        label_bytes,
        project_bytes,
        semantic_rows,
        semantic_key_bytes,
        semantic_value_bytes,
        evidence_rows,
        evidence_value_bytes,
        largest_evidence_value,
        project,
        owner,
        operation_key,
    })
}

/// Opaque owner for a decoded V17 intent and its admission reservation.
/// No reference to the cloneable inner intent is exposed. Future replay methods
/// must operate here and separately meter any retained output.
#[allow(dead_code)]
pub(super) struct DecodedRetirementIntent {
    wire: super::native_ts_retirement_pointer::RetirementWire,
    owner: [u8; 32],
    operation_key: Option<[u8; 32]>,
}

#[allow(dead_code)]
impl DecodedRetirementIntent {
    /// The owner coordinate is a fixed-size scalar; no decoded row escapes.
    pub(super) fn owner(&self) -> [u8; 32] {
        self.owner
    }

    /// The durable operation identity, when the wire carried one.
    pub(super) fn operation_key(&self) -> Option<[u8; 32]> {
        self.operation_key
    }
}

/// Parse the narrow V17 shape into an opaque, non-authoritative wire carrier. The input
/// must be charged to the same admission. Replay remains separately guarded.
/// No caller can supply an arbitrary metered value to manufacture the carrier.
#[allow(dead_code)]
pub(super) fn decode_retirement_intent(
    input: super::native_ts_retirement_pointer::RetirementWire,
    budget: &backend_version::retirement_budget::RetirementBudget,
    bookkeeping: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<DecodedRetirementIntent, &'static str> {
    if !input.covered_by(budget, bookkeeping) {
        return Err("retirement intent input is not covered by this budget");
    }
    let bytes = input.bytes();
    let summary = retirement_intent_wire_summary(bytes, cancelled)?;
    // Mirror the generic decoder's resident ceiling so a compact carrier never
    // accepts an intent that the existing V17 grammar would reject there.
    let resident = summary
        .evidence_rows
        .checked_mul(std::mem::size_of::<super::profile::NativeEvidenceChange>())
        .and_then(|n| n.checked_add(summary.evidence_value_bytes))
        .and_then(|n| n.checked_add(summary.evidence_rows.checked_mul(128)?))
        .ok_or("evidence resident overflow")?;
    if resident > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
        return Err("evidence resident limit");
    }
    let owner = summary.owner;
    let operation_key = summary.operation_key;
    if cancelled() {
        return Err("cancelled");
    }
    Ok(DecodedRetirementIntent {
        wire: input,
        owner,
        operation_key,
    })
}
