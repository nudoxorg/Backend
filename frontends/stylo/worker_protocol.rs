//! Strict, bounded protocol for the supervised Stylo worker.
use crate::{StyloError, analyze_embedded_with_cancel, analyze_with_cancel};
use backend_compile::{VerifiedSourceExtent, VerifiedSourceText};
use backend_semantic::stylo_analysis::{StyloAnalysis, StyloParseConfig, StyloSourceKind};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

const REQUEST_MAGIC: &[u8; 8] = b"STYLOR01";
const RESPONSE_MAGIC: &[u8; 8] = b"STYLOS01";
const FRAGMENT_REQUEST_MAGIC: &[u8; 8] = b"STYLOR02";
const FRAGMENT_RESPONSE_MAGIC: &[u8; 8] = b"STYLOS02";
/// Maximum encoded configuration bytes.
pub const MAX_CONFIG: usize = 16 * 1024;
/// Maximum standalone source bytes.
pub const MAX_SOURCE: usize = 4 * 1024 * 1024;
/// Maximum encoded response body bytes (excluding its 12-byte header).
pub const MAX_OUTPUT: usize = 8 * 1024 * 1024;
/// Maximum complete request-frame bytes.
pub const MAX_INPUT: usize = 16 + MAX_CONFIG + MAX_SOURCE;

/// Framing, supervision or native worker failure. No variant means complete empty.
#[derive(Debug)]
pub enum WorkerError {
    /// Invalid, malformed, out-of-bounds or inconsistent frame.
    Protocol,
    /// The worker reported an analysis error.
    Analysis(StyloError),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    source_digest: [u8; 32],
    result: Result<StyloAnalysis, StyloError>,
}
fn valid_limits(config: &StyloParseConfig, source_len: usize) -> bool {
    let l = &config.limits;
    config.base_url.len() <= 8 * 1024
        && source_len <= MAX_SOURCE
        && l.input_bytes <= MAX_SOURCE as u64
        && source_len as u64 <= l.input_bytes
        && l.rules <= 100_000
        && l.depth <= 64
        && l.records <= 200_000
        && l.diagnostics <= 2_000
        && l.text_bytes <= 8 * 1024 * 1024
}
/// Encode one bounded exact source/configuration request for the worker.
///
/// # Errors
/// Rejects configured budgets or input outside the worker's fixed caps.
pub fn encode_request(source: &[u8], config: &StyloParseConfig) -> Result<Vec<u8>, WorkerError> {
    if config.source_kind != StyloSourceKind::StandaloneUtf8 || !valid_limits(config, source.len())
    {
        return Err(WorkerError::Protocol);
    }
    let config = serde_json::to_vec(config).map_err(|_| WorkerError::Protocol)?;
    if config.len() > MAX_CONFIG {
        return Err(WorkerError::Protocol);
    }
    let mut bytes = Vec::with_capacity(16 + config.len() + source.len());
    bytes.extend_from_slice(REQUEST_MAGIC);
    bytes.extend_from_slice(&(config.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&(source.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&config);
    bytes.extend_from_slice(source);
    Ok(bytes)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FragmentHeader {
    config: StyloParseConfig,
    start: usize,
    end: usize,
    input_digest: [u8; 32],
    source_digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FragmentResponse {
    input_digest: [u8; 32],
    source_digest: [u8; 32],
    start: usize,
    end: usize,
    result: Result<StyloAnalysis, StyloError>,
}
/// Encode the full verified host and one exact embedded region for the V2 worker.
/// A partial host extent or incompatible profile is rejected.
pub fn encode_fragment_request(
    host: &VerifiedSourceExtent<'_>,
    region: std::ops::Range<usize>,
    config: &StyloParseConfig,
) -> Result<Vec<u8>, WorkerError> {
    let bytes = host.text().as_bytes();
    let captured = VerifiedSourceText::capture(bytes).map_err(|_| WorkerError::Protocol)?;
    let whole = captured
        .extent(0..bytes.len())
        .map_err(|_| WorkerError::Protocol)?;
    if host.byte_range().start != 0
        || whole.content() != host.content()
        || whole.source() != host.source()
        || config.source_kind != StyloSourceKind::EmbeddedUnicodeFragment
        || bytes.len() > MAX_SOURCE
    {
        return Err(WorkerError::Protocol);
    }
    let fragment = host
        .subregion(region.clone())
        .map_err(|_| WorkerError::Protocol)?;
    if !valid_limits(config, fragment.text().len()) {
        return Err(WorkerError::Protocol);
    }
    let header = FragmentHeader {
        config: config.clone(),
        start: region.start,
        end: region.end,
        input_digest: host.content().to_bytes(),
        source_digest: *host.source().as_ref(),
    };
    let header = serde_json::to_vec(&header).map_err(|_| WorkerError::Protocol)?;
    if header.len() > MAX_CONFIG {
        return Err(WorkerError::Protocol);
    }
    let mut output = Vec::with_capacity(16 + header.len() + bytes.len());
    output.extend_from_slice(FRAGMENT_REQUEST_MAGIC);
    output.extend_from_slice(&(header.len() as u32).to_be_bytes());
    output.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    output.extend_from_slice(&header);
    output.extend_from_slice(bytes);
    Ok(output)
}
/// A verified fragment analysis. Native occurrence offsets remain relative to
/// `fragment`; callers must map them with `host_occurrence`.
pub struct VerifiedFragmentAnalysis<'a> {
    analysis: StyloAnalysis,
    fragment: VerifiedSourceExtent<'a>,
}
impl<'a> VerifiedFragmentAnalysis<'a> {
    /// Bounded native output; its offsets are fragment-relative.
    pub fn analysis(&self) -> &StyloAnalysis {
        &self.analysis
    }
    /// Map one native occurrence to exact original host bytes.
    pub fn host_occurrence(&self, index: usize) -> Option<VerifiedSourceExtent<'a>> {
        let occurrence = self.analysis.occurrences.get(index)?;
        let extent = self
            .fragment
            .subregion(occurrence.start as usize..occurrence.end as usize)
            .ok()?;
        (extent.text() == occurrence.raw).then_some(extent)
    }
}
/// Admit one V2 response against exact host, region, configuration and support identity.
pub fn decode_fragment_response<'a>(
    bytes: &[u8],
    host: &VerifiedSourceExtent<'a>,
    region: std::ops::Range<usize>,
    config: &StyloParseConfig,
    expected_support: [u8; 32],
) -> Result<VerifiedFragmentAnalysis<'a>, WorkerError> {
    // Re-run request validation, including full-host identity verification.
    let _ = encode_fragment_request(host, region.clone(), config)?;
    let body = take_frame(bytes, FRAGMENT_RESPONSE_MAGIC, MAX_OUTPUT)?;
    preflight(body, config)?;
    let response: FragmentResponse =
        serde_json::from_slice(body).map_err(|_| WorkerError::Protocol)?;
    if response.input_digest != host.content().to_bytes()
        || response.source_digest != *host.source().as_ref()
        || response.start != region.start
        || response.end != region.end
    {
        return Err(WorkerError::Protocol);
    }
    let fragment = host.subregion(region).map_err(|_| WorkerError::Protocol)?;
    let analysis = response.result.map_err(WorkerError::Analysis)?;
    if analysis.config != *config
        || analysis.support_fingerprint != expected_support
        || analysis.utf8_bom_removed
        || analysis.recipe != crate::EMBEDDED_RECIPE
        || analysis.source_observer_recipe
            != "stylo-observer-top-level-v1;native-class-token;accepted-custom-name;no-bom-removal;disposition=unknown"
    {
        return Err(WorkerError::Protocol);
    }
    validate_analysis(&analysis, fragment.text().as_bytes())?;
    Ok(VerifiedFragmentAnalysis { analysis, fragment })
}
fn take_frame<'a>(bytes: &'a [u8], magic: &[u8; 8], max: usize) -> Result<&'a [u8], WorkerError> {
    if bytes.len() < 12 || &bytes[..8] != magic {
        return Err(WorkerError::Protocol);
    }
    let n =
        u32::from_be_bytes(bytes[8..12].try_into().map_err(|_| WorkerError::Protocol)?) as usize;
    if n > max || bytes.len() != 12usize.checked_add(n).ok_or(WorkerError::Protocol)? {
        return Err(WorkerError::Protocol);
    }
    Ok(&bytes[12..])
}
// A non-allocating structural pass rejects over-budget arrays before serde can
// grow Vecs. Keys are unescaped ASCII because this private protocol's encoder
// emits fixed schema field names; accepting escaped aliases would evade limits.
struct Preflight<'a> {
    bytes: &'a [u8],
    at: usize,
    values: usize,
    max_values: usize,
    limits: &'a backend_semantic::stylo_analysis::StyloLimits,
}
impl<'a> Preflight<'a> {
    fn ws(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn byte(&mut self, wanted: u8) -> Result<(), WorkerError> {
        self.ws();
        if self.bytes.get(self.at) != Some(&wanted) {
            return Err(WorkerError::Protocol);
        }
        self.at += 1;
        Ok(())
    }
    fn string(&mut self, key: bool) -> Result<&'a [u8], WorkerError> {
        self.byte(b'"')?;
        let start = self.at;
        loop {
            let b = *self.bytes.get(self.at).ok_or(WorkerError::Protocol)?;
            self.at += 1;
            match b {
                b'"' => return Ok(&self.bytes[start..self.at - 1]),
                b'\\' if key => return Err(WorkerError::Protocol),
                b'\\' => {
                    self.at = self.at.checked_add(1).ok_or(WorkerError::Protocol)?;
                    if self.at > self.bytes.len() {
                        return Err(WorkerError::Protocol);
                    }
                }
                0..=31 => return Err(WorkerError::Protocol),
                _ => {}
            }
        }
    }
    fn array_limit(&self, key: &[u8]) -> usize {
        match key {
            b"rules" => self.limits.rules as usize,
            b"selectors" | b"declarations" | b"dependencies" | b"occurrences" | b"components" => {
                self.limits.records as usize
            }
            b"diagnostics" => self.limits.diagnostics as usize,
            b"path" | b"rule_path" | b"reference_path" => self.limits.depth as usize,
            b"source_gaps" => 4,
            b"source_digest" | b"input_digest" | b"support_fingerprint" => 32,
            b"booleans" | b"integers" => 4096,
            _ => 4096,
        }
    }
    fn value(&mut self, key: &[u8], depth: usize) -> Result<(), WorkerError> {
        if depth > 80 || self.values >= self.max_values {
            return Err(WorkerError::Protocol);
        }
        self.values += 1;
        self.ws();
        match self
            .bytes
            .get(self.at)
            .copied()
            .ok_or(WorkerError::Protocol)?
        {
            b'{' => {
                self.at += 1;
                self.ws();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(());
                }
                let mut fields = 0usize;
                loop {
                    fields += 1;
                    if fields > 64 {
                        return Err(WorkerError::Protocol);
                    }
                    let key = self.string(true)?;
                    self.byte(b':')?;
                    self.value(key, depth + 1)?;
                    self.ws();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(());
                        }
                        _ => return Err(WorkerError::Protocol),
                    }
                }
            }
            b'[' => {
                self.at += 1;
                self.ws();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(());
                }
                let mut count = 0usize;
                let limit = self.array_limit(key);
                loop {
                    count += 1;
                    if count > limit {
                        return Err(WorkerError::Protocol);
                    }
                    self.value(b"", depth + 1)?;
                    self.ws();
                    match self.bytes.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(());
                        }
                        _ => return Err(WorkerError::Protocol),
                    }
                }
            }
            b'"' => {
                self.string(false)?;
                Ok(())
            }
            _ => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b',' | b']' | b'}'))
                {
                    self.at += 1;
                }
                if self.at == start {
                    Err(WorkerError::Protocol)
                } else {
                    Ok(())
                }
            }
        }
    }
}
fn preflight(body: &[u8], config: &StyloParseConfig) -> Result<(), WorkerError> {
    let l = &config.limits;
    let max_values = (l.rules as usize)
        .saturating_add(l.records as usize)
        .saturating_add(l.diagnostics as usize)
        .saturating_mul(40)
        .saturating_add(10_000)
        .min(1_000_000);
    let mut scan = Preflight {
        bytes: body,
        at: 0,
        values: 0,
        max_values,
        limits: l,
    };
    scan.value(b"", 0)?;
    scan.ws();
    if scan.at != body.len() {
        return Err(WorkerError::Protocol);
    }
    Ok(())
}
fn validate_analysis(analysis: &StyloAnalysis, source: &[u8]) -> Result<(), WorkerError> {
    let l = &analysis.config.limits;
    let component_count = analysis
        .selectors
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.components.len()))
        .ok_or(WorkerError::Protocol)?;
    let total_records = analysis
        .selectors
        .len()
        .checked_add(analysis.declarations.len())
        .and_then(|n| n.checked_add(analysis.dependencies.len()))
        .and_then(|n| n.checked_add(analysis.occurrences.len()))
        .and_then(|n| n.checked_add(component_count))
        .ok_or(WorkerError::Protocol)?;
    if analysis.rules.len() > l.rules as usize
        || total_records > l.records as usize
        || analysis.diagnostics.len() > l.diagnostics as usize
        || analysis.source_gaps.len() > 4
        || analysis.preferences.booleans.len() > 4096
        || analysis.preferences.integers.len() > 4096
    {
        return Err(WorkerError::Protocol);
    }
    let depth = l.depth as usize;
    if analysis.rules.iter().any(|r| r.path.len() > depth)
        || analysis.selectors.iter().any(|s| s.rule_path.len() > depth)
        || analysis
            .declarations
            .iter()
            .any(|d| d.rule_path.len() > depth)
        || analysis
            .dependencies
            .iter()
            .any(|d| d.rule_path.len() > depth || d.reference_path.len() > depth)
        || analysis
            .occurrences
            .iter()
            .any(|o| o.rule_path.len() > depth)
    {
        return Err(WorkerError::Protocol);
    }
    let mut text_bytes = 0u64;
    let mut add_text = |s: &str| -> Result<(), WorkerError> {
        text_bytes = text_bytes
            .checked_add(s.len() as u64)
            .ok_or(WorkerError::Protocol)?;
        if text_bytes > l.text_bytes {
            return Err(WorkerError::Protocol);
        }
        Ok(())
    };
    for r in &analysis.rules {
        add_text(&r.native_kind)?;
        if let Some(c) = &r.condition {
            add_text(c)?;
        }
    }
    for s in &analysis.selectors {
        add_text(&s.canonical)?;
        for c in &s.components {
            add_text(&c.name)?;
        }
    }
    for d in &analysis.declarations {
        add_text(&d.property)?;
        add_text(&d.value)?;
    }
    for d in &analysis.dependencies {
        add_text(&d.name)?;
        add_text(&d.kind)?;
    }
    for d in &analysis.diagnostics {
        add_text(&d.message)?;
    }
    for o in &analysis.occurrences {
        add_text(&o.raw)?;
        add_text(&o.decoded)?;
    }
    let source = std::str::from_utf8(source).map_err(|_| WorkerError::Protocol)?;
    for occurrence in &analysis.occurrences {
        let start = usize::try_from(occurrence.start).map_err(|_| WorkerError::Protocol)?;
        let end = usize::try_from(occurrence.end).map_err(|_| WorkerError::Protocol)?;
        if source.get(start..end) != Some(occurrence.raw.as_str()) {
            return Err(WorkerError::Protocol);
        }
    }
    Ok(())
}
/// Decode and verify one complete worker response against the captured source,
/// requested parser configuration and trusted selected producer fingerprint.
/// Process supervision, executable identity, terminal state and reaping must be
/// admitted separately by the caller before using this result.
///
/// # Errors
/// Rejects malformed/trailing frames, wrong source/configuration/fingerprint,
/// and reports a typed native analysis failure.
pub fn decode_response(
    bytes: &[u8],
    source: &[u8],
    config: &StyloParseConfig,
    expected_support: [u8; 32],
) -> Result<StyloAnalysis, WorkerError> {
    if config.source_kind != StyloSourceKind::StandaloneUtf8 {
        return Err(WorkerError::Protocol);
    }
    let body = take_frame(bytes, RESPONSE_MAGIC, MAX_OUTPUT)?;
    preflight(body, config)?;
    let response: Response = serde_json::from_slice(body).map_err(|_| WorkerError::Protocol)?;
    if response.source_digest != *blake3::hash(source).as_bytes() {
        return Err(WorkerError::Protocol);
    }
    let analysis = response.result.map_err(WorkerError::Analysis)?;
    if analysis.config != *config || analysis.support_fingerprint != expected_support {
        return Err(WorkerError::Protocol);
    }
    validate_analysis(&analysis, source)?;
    Ok(analysis)
}
struct Limited {
    bytes: Vec<u8>,
    limit: usize,
    overflow: bool,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.overflow = true;
            return Err(io::Error::other("output limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode_response(response: &Response, limit: usize) -> Result<Vec<u8>, WorkerError> {
    let mut encoded = Limited {
        bytes: Vec::new(),
        limit,
        overflow: false,
    };
    if serde_json::to_writer(&mut encoded, response).is_err() {
        if !encoded.overflow {
            return Err(WorkerError::Protocol);
        }
        // Discard the incomplete body and return a bounded, explicit limit terminal.
        let fallback = Response {
            source_digest: response.source_digest,
            result: Err(StyloError::Limit),
        };
        encoded = Limited {
            bytes: Vec::new(),
            limit,
            overflow: false,
        };
        serde_json::to_writer(&mut encoded, &fallback).map_err(|_| WorkerError::Protocol)?;
    }
    Ok(encoded.bytes)
}
/// Run one request from standard input to standard output in a worker process.
///
/// # Errors
/// Rejects malformed/trailing/oversized input. If native output exceeds the
/// body cap, emits a small typed limit terminal rather than a partial result.
pub fn serve(mut input: impl Read, mut output: impl Write) -> Result<(), WorkerError> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| WorkerError::Protocol)?;
    if bytes.len() >= 8 && &bytes[..8] == FRAGMENT_REQUEST_MAGIC {
        return serve_fragment(&bytes, &mut output);
    }
    if bytes.len() < 16 || bytes.len() > MAX_INPUT || &bytes[..8] != REQUEST_MAGIC {
        return Err(WorkerError::Protocol);
    }
    let config_len =
        u32::from_be_bytes(bytes[8..12].try_into().map_err(|_| WorkerError::Protocol)?) as usize;
    let source_len = u32::from_be_bytes(
        bytes[12..16]
            .try_into()
            .map_err(|_| WorkerError::Protocol)?,
    ) as usize;
    let end_config = 16usize
        .checked_add(config_len)
        .ok_or(WorkerError::Protocol)?;
    if config_len > MAX_CONFIG
        || source_len > MAX_SOURCE
        || bytes.len()
            != end_config
                .checked_add(source_len)
                .ok_or(WorkerError::Protocol)?
    {
        return Err(WorkerError::Protocol);
    }
    let config: StyloParseConfig =
        serde_json::from_slice(&bytes[16..end_config]).map_err(|_| WorkerError::Protocol)?;
    let source = &bytes[end_config..];
    if config.source_kind != StyloSourceKind::StandaloneUtf8 || !valid_limits(&config, source.len())
    {
        return Err(WorkerError::Protocol);
    }
    let result = analyze_with_cancel(source, config, || false);
    let response = Response {
        source_digest: *blake3::hash(source).as_bytes(),
        result,
    };
    let encoded = encode_response(&response, MAX_OUTPUT)?;
    output
        .write_all(RESPONSE_MAGIC)
        .map_err(|_| WorkerError::Protocol)?;
    output
        .write_all(&(encoded.len() as u32).to_be_bytes())
        .map_err(|_| WorkerError::Protocol)?;
    output
        .write_all(&encoded)
        .map_err(|_| WorkerError::Protocol)?;
    output.flush().map_err(|_| WorkerError::Protocol)
}

fn serve_fragment(bytes: &[u8], mut output: impl Write) -> Result<(), WorkerError> {
    if bytes.len() < 16 || bytes.len() > MAX_INPUT {
        return Err(WorkerError::Protocol);
    }
    let header_len =
        u32::from_be_bytes(bytes[8..12].try_into().map_err(|_| WorkerError::Protocol)?) as usize;
    let host_len = u32::from_be_bytes(
        bytes[12..16]
            .try_into()
            .map_err(|_| WorkerError::Protocol)?,
    ) as usize;
    let end_header = 16usize
        .checked_add(header_len)
        .ok_or(WorkerError::Protocol)?;
    if header_len > MAX_CONFIG
        || host_len > MAX_SOURCE
        || end_header.checked_add(host_len) != Some(bytes.len())
    {
        return Err(WorkerError::Protocol);
    }
    let header: FragmentHeader =
        serde_json::from_slice(&bytes[16..end_header]).map_err(|_| WorkerError::Protocol)?;
    let verified =
        VerifiedSourceText::capture(&bytes[end_header..]).map_err(|_| WorkerError::Protocol)?;
    let host = verified
        .extent(0..host_len)
        .map_err(|_| WorkerError::Protocol)?;
    if host.content().to_bytes() != header.input_digest
        || *host.source().as_ref() != header.source_digest
        || header.config.source_kind != StyloSourceKind::EmbeddedUnicodeFragment
    {
        return Err(WorkerError::Protocol);
    }
    let fragment = host
        .subregion(header.start..header.end)
        .map_err(|_| WorkerError::Protocol)?;
    if !valid_limits(&header.config, fragment.text().len()) {
        return Err(WorkerError::Protocol);
    }
    let response = FragmentResponse {
        input_digest: header.input_digest,
        source_digest: header.source_digest,
        start: header.start,
        end: header.end,
        result: analyze_embedded_with_cancel(&fragment, header.config, || false),
    };
    let mut encoded = Limited {
        bytes: Vec::new(),
        limit: MAX_OUTPUT,
        overflow: false,
    };
    if serde_json::to_writer(&mut encoded, &response).is_err() {
        if !encoded.overflow {
            return Err(WorkerError::Protocol);
        }
        encoded = Limited {
            bytes: Vec::new(),
            limit: MAX_OUTPUT,
            overflow: false,
        };
        let fallback = FragmentResponse {
            result: Err(StyloError::Limit),
            ..response
        };
        serde_json::to_writer(&mut encoded, &fallback).map_err(|_| WorkerError::Protocol)?;
    }
    output
        .write_all(FRAGMENT_RESPONSE_MAGIC)
        .map_err(|_| WorkerError::Protocol)?;
    output
        .write_all(&(encoded.bytes.len() as u32).to_be_bytes())
        .map_err(|_| WorkerError::Protocol)?;
    output
        .write_all(&encoded.bytes)
        .map_err(|_| WorkerError::Protocol)?;
    output.flush().map_err(|_| WorkerError::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::stylo_analysis::{StyloLimits, StyloSourceKind};
    fn config() -> StyloParseConfig {
        StyloParseConfig {
            source_kind: StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/x.css".into(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 1000,
                rules: 100,
                depth: 16,
                records: 100,
                diagnostics: 10,
                text_bytes: 10000,
            },
        }
    }
    #[test]
    fn fragment_roundtrip_binds_host_offsets_and_preserves_leading_feff() {
        let text = "\u{feff}<!-- π 🚀 -->\r\n<style>\u{feff} {} .card { --x: 1; }</style>\r\n<style>\u{feff} {} .card { --x: 1; }</style>";
        let verified = VerifiedSourceText::capture(text.as_bytes()).unwrap();
        let host = verified.extent(0..text.len()).unwrap();
        let start = text.find("\u{feff} {} .card").unwrap();
        let end = start + "\u{feff} {} .card { --x: 1; }".len();
        let mut config = config();
        config.source_kind = StyloSourceKind::EmbeddedUnicodeFragment;
        assert!(encode_request(&text.as_bytes()[start..end], &config).is_err());
        let request = encode_fragment_request(&host, start..end, &config).unwrap();
        let mut output = Vec::new();
        serve(request.as_slice(), &mut output).unwrap();
        let result = decode_fragment_response(
            &output,
            &host,
            start..end,
            &config,
            crate::support_fingerprint(),
        )
        .unwrap();
        assert!(!result.analysis().utf8_bom_removed);
        assert_eq!(result.analysis().recipe, crate::EMBEDDED_RECIPE);
        let i = result
            .analysis()
            .occurrences
            .iter()
            .position(|o| o.raw == "card")
            .unwrap();
        let extent = result.host_occurrence(i).unwrap();
        assert_eq!(extent.text(), "card");
        assert_eq!(&text[extent.byte_range()], "card");
        let second = text.rfind("\u{feff} {} .card").unwrap();
        assert!(
            decode_fragment_response(
                &output,
                &host,
                second..second + end - start,
                &config,
                crate::support_fingerprint()
            )
            .is_err()
        );
        let edited = text.replace("π", "ω");
        let other = VerifiedSourceText::capture(edited.as_bytes()).unwrap();
        let other = other.extent(0..edited.len()).unwrap();
        assert!(
            decode_fragment_response(
                &output,
                &other,
                start..end,
                &config,
                crate::support_fingerprint()
            )
            .is_err()
        );
        assert!(encode_fragment_request(&host, start + 1..end, &config).is_err());
    }
    #[test]
    fn preflight_rejects_array_and_depth_budget_evasion() {
        let mut config = config();
        config.limits.rules = 1;
        config.limits.depth = 1;
        assert!(preflight(br#"{"rules":[0,0]}"#, &config).is_err());
        assert!(preflight(br#"{"rule_path":[0,0]}"#, &config).is_err());
        assert!(preflight(br#"{"ru\u006ces":[0,0]}"#, &config).is_err());
        assert!(preflight(br#"{"rules":[0]}"#, &config).is_ok());
        let nested = format!("{}0{}", "[".repeat(81), "]".repeat(81));
        assert!(preflight(nested.as_bytes(), &config).is_err());
    }
    #[test]
    fn oversized_response_becomes_small_explicit_limit_terminal() {
        let config = config();
        let source = b".hud-menu { --x: 1px; }";
        let response = Response {
            source_digest: *blake3::hash(source).as_bytes(),
            result: analyze_with_cancel(source, config, || false),
        };
        let body = encode_response(&response, 256).unwrap();
        let parsed: Response = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.source_digest, *blake3::hash(source).as_bytes());
        assert!(matches!(parsed.result, Err(StyloError::Limit)));
        assert!(body.len() <= 256);
    }
    #[test]
    fn exact_roundtrip_and_changed_basis_are_rejected() {
        let source = b".hud-menu { --x: 1px; }";
        let config = config();
        let input = encode_request(source, &config).unwrap();
        let mut output = Vec::new();
        serve(input.as_slice(), &mut output).unwrap();
        let fingerprint = crate::support_fingerprint();
        let result = decode_response(&output, source, &config, fingerprint).unwrap();
        assert_eq!(result.config, config);
        assert!(result.occurrences.iter().any(|o| o.raw == "hud-menu"));
        assert!(result.occurrences.iter().any(|o| o.raw == "--x"));
        assert!(matches!(
            decode_response(&output, b".bad-menu { --x: 1px; }", &config, fingerprint),
            Err(WorkerError::Protocol)
        ));
        assert!(matches!(
            decode_response(&output, source, &config, [0; 32]),
            Err(WorkerError::Protocol)
        ));
        let mut other = config.clone();
        other.base_url.push('x');
        assert!(matches!(
            decode_response(&output, source, &other, fingerprint),
            Err(WorkerError::Protocol)
        ));
        let mut trailing = output.clone();
        trailing.push(0);
        assert!(matches!(
            decode_response(&trailing, source, &config, fingerprint),
            Err(WorkerError::Protocol)
        ));
        let mut corrupt = input.clone();
        corrupt.push(0);
        assert!(serve(corrupt.as_slice(), Vec::new()).is_err());
        assert!(serve(&input[..15], Vec::new()).is_err());
        let mut limits = config;
        limits.limits.input_bytes = (MAX_SOURCE + 1) as u64;
        assert!(encode_request(source, &limits).is_err());
    }
}
