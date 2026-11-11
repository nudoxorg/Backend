//! Strict claim-only replay of a staged C# invocation aggregate.
//! Decoding never selects a compiler publication, a source row, or an image.

use backend_semantic::vocabulary::LanguageProfile;
use backend_version::{ContentId, SourceFactDomain};
use sha2::{Digest, Sha256};

use super::compiler::csharp_config_probe_paths;
use crate::compiler_input_manifest_v2::CompilationUnitKeyV2;

/// One captured configuration presence or absence claim.
pub struct CSharpEvidenceConfig<'a> {
    pub path: &'a str,
    pub text: Option<&'a str>,
}
/// One claim linking a captured source, invocation and lowered image.
pub struct CSharpEvidenceSource<'a> {
    pub path: &'a str,
    pub source_fact: [u8; 32],
    pub byte_length: u32,
    pub input_content: [u8; 32],
    pub source_sha256: [u8; 32],
    pub image_identity: [u8; 32],
    pub image_length: u32,
    pub sidecar: &'a [u8],
}
/// Bounded decoded claims. They do not establish capture or selection authority.
pub struct CSharpEvidenceClaim<'a> {
    pub target: [u8; 32],
    pub input: [u8; 32],
    pub profile: LanguageProfile,
    pub project_path: &'a str,
    pub config: Vec<CSharpEvidenceConfig<'a>>,
    pub sources: Vec<CSharpEvidenceSource<'a>>,
}

/// Versioned graph evidence carried separately from legacy CSE1 records.
/// CSE2 is claim-only until the owner revalidates every digest against its
/// same-attempt captured graph and targeting snapshot.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CSharpGraphEvidence {
    pub root_project: String,
    pub config_digest: [u8; 32],
    pub source_digest: [u8; 32],
    pub reference_digest: [u8; 32],
    #[serde(deserialize_with = "bounded_graph_nodes")]
    pub nodes: Vec<CSharpGraphEvidenceNode>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CSharpGraphEvidenceNode {
    pub project_path: String,
    #[serde(deserialize_with = "bounded_graph_edges")]
    pub project_references: Vec<String>,
    pub config_digest: [u8; 32],
    pub source_digest: [u8; 32],
}

fn bounded_graph_nodes<'de, D>(deserializer: D) -> Result<Vec<CSharpGraphEvidenceNode>, D::Error>
where D: serde::Deserializer<'de> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<CSharpGraphEvidenceNode>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("at most 64 graph nodes") }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element()? { if values.len() >= 64 { return Err(serde::de::Error::custom("graph node limit")); } values.push(value); }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

fn bounded_graph_edges<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where D: serde::Deserializer<'de> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("at most 64 graph edges") }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? { if values.len() >= 64 || value.len() > 4096 { return Err(serde::de::Error::custom("graph edge limit")); } values.push(value); }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

/// Durable owner relation binding graph evidence to one selected operation and
/// workspace snapshot. This relation is claim storage only; grant still needs
/// the private selected-witness mint.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DurableCSharpGraphRelation {
    pub format: u8,
    pub operation: [u8; 32],
    pub attempt: [u8; 32],
    pub owner_epoch: u64,
    pub snapshot: [u8; 32],
    pub graph: CSharpGraphEvidence,
}

pub fn encode_durable_csharp_graph_relation(relation: &DurableCSharpGraphRelation) -> Option<Vec<u8>> {
    if relation.format != 1 { return None; }
    let payload = serde_json::to_vec(relation).ok()?;
    (payload.len() <= 2 * 1024 * 1024).then(|| payload)
}

pub fn decode_durable_csharp_graph_relation(bytes: &[u8]) -> Option<DurableCSharpGraphRelation> {
    if bytes.len() > 2 * 1024 * 1024 { return None; }
    let relation = serde_json::from_slice::<DurableCSharpGraphRelation>(bytes).ok()?;
    if relation.format != 1 || relation.operation == [0; 32] || relation.attempt == [0; 32] || relation.snapshot == [0; 32] || encode_csharp_graph_evidence(&relation.graph).is_none() { return None; }
    Some(relation)
}

/// Owner-minted selected witness. The graph payload is private; callers can
/// obtain one only by joining staged evidence with a separately selected
/// same-attempt snapshot through `mint_selected_graph_witness`.
pub struct SelectedCSharpGraphWitness(CSharpGraphEvidence);

fn mint_selected_graph_witness(
    staged: &CSharpGraphEvidence,
    selected: CSharpGraphEvidence,
) -> Option<SelectedCSharpGraphWitness> {
    replay_csharp_graph_evidence(staged, &selected).then_some(SelectedCSharpGraphWitness(selected))
}

pub(crate) fn selected_graph_evidence(witness: &SelectedCSharpGraphWitness) -> &CSharpGraphEvidence {
    &witness.0
}

pub fn encode_csharp_graph_evidence(graph: &CSharpGraphEvidence) -> Option<Vec<u8>> {
    if graph.nodes.is_empty() || graph.nodes.len() > 64 || graph.root_project.len() > 4096
        || graph.nodes.windows(2).any(|pair| pair[0].project_path >= pair[1].project_path)
        || graph.nodes.iter().any(|node| node.project_path.len() > 4096 || node.project_references.len() > 64)
        || graph.nodes.iter().try_fold(0usize, |total, node| total.checked_add(node.project_references.len())).is_none_or(|edges| edges > 256)
    { return None; }
    let payload = serde_json::to_vec(graph).ok()?;
    if payload.len() > 2 * 1024 * 1024 { return None; }
    let mut framed = b"CSE2".to_vec();
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);
    Some(framed)
}

pub fn decode_csharp_graph_evidence(bytes: &[u8]) -> Option<CSharpGraphEvidence> {
    if bytes.len() < 8 || &bytes[..4] != b"CSE2" { return None; }
    let length = u32::from_be_bytes(bytes[4..8].try_into().ok()?) as usize;
    if length > 2 * 1024 * 1024 || bytes.len() != length.checked_add(8)? { return None; }
    let graph = serde_json::from_slice::<CSharpGraphEvidence>(&bytes[8..]).ok()?;
    if graph.nodes.is_empty() || graph.nodes.len() > 64 || graph.root_project.len() > 4096
        || graph.nodes.windows(2).any(|pair| pair[0].project_path >= pair[1].project_path)
        || graph.nodes.iter().any(|node| node.project_path.len() > 4096 || node.project_references.len() > 64 || node.project_references.iter().any(|path| path.len() > 4096))
        || graph.nodes.iter().try_fold(0usize, |total, node| total.checked_add(node.project_references.len())).is_none_or(|edges| edges > 256)
        || !graph.nodes.iter().any(|node| node.project_path == graph.root_project)
    { return None; }
    let mut paths = std::collections::BTreeSet::new();
    for node in &graph.nodes {
        if !paths.insert(node.project_path.as_str())
            || node.project_references.windows(2).any(|pair| pair[0] >= pair[1])
            || node.project_references.iter().any(|path| !graph.nodes.iter().any(|candidate| candidate.project_path == *path))
        { return None; }
    }
    Some(graph)
}

/// Replays graph evidence only when every same-attempt digest and canonical
/// node/edge set matches the owner-captured graph. This does not promote the
/// claim to authority; the caller still checks SDK/helper/runtime custody.
pub fn replay_csharp_graph_evidence(expected: &CSharpGraphEvidence, actual: &CSharpGraphEvidence) -> bool {
    expected == actual
}

#[cfg(test)]
mod graph_evidence_tests {
    use super::*;

    #[test]
    fn cse2_round_trip_and_truncation_refusal() {
        let graph = CSharpGraphEvidence {
            root_project: "Tests/Tests.csproj".into(),
            config_digest: [1; 32], source_digest: [2; 32], reference_digest: [3; 32],
            nodes: vec![CSharpGraphEvidenceNode {
                project_path: "Api/Api.csproj".into(), project_references: Vec::new(),
                config_digest: [6; 32], source_digest: [7; 32],
            }, CSharpGraphEvidenceNode {
                project_path: "Tests/Tests.csproj".into(), project_references: vec!["Api/Api.csproj".into()],
                config_digest: [4; 32], source_digest: [5; 32],
            }],
        };
        let encoded = encode_csharp_graph_evidence(&graph).expect("bounded CSE2");
        assert_eq!(decode_csharp_graph_evidence(&encoded), Some(graph.clone()));
        assert!(decode_csharp_graph_evidence(&encoded[..encoded.len() - 1]).is_none());
        let mut changed = graph.clone();
        changed.nodes[1].project_references.clear();
        assert!(!replay_csharp_graph_evidence(&graph, &changed));
        for mutate in [
            |g: &mut CSharpGraphEvidence| g.config_digest[0] ^= 1,
            |g: &mut CSharpGraphEvidence| g.nodes[0].source_digest[0] ^= 1,
            |g: &mut CSharpGraphEvidence| g.reference_digest[0] ^= 1,
        ] {
            let mut changed = graph.clone();
            mutate(&mut changed);
            assert!(!replay_csharp_graph_evidence(&graph, &changed));
        }
        assert!(mint_selected_graph_witness(&graph, graph.clone()).is_some());
        let mut stale = graph.clone();
        stale.nodes[1].config_digest[0] ^= 1;
        assert!(mint_selected_graph_witness(&graph, stale).is_none());
        let mut oversized = graph.clone();
        oversized.nodes[0].project_references = (0..65).map(|_| "".to_owned()).collect();
        let encoded = encode_csharp_graph_evidence(&oversized);
        assert!(encoded.is_none());
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sidecar {
    format: u8,
    nonce: String,
    source_binding: String,
    image_sha256: String,
    image_length: u64,
    config_sha256: String,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    sources: Vec<SidecarSource>,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    references: Vec<SidecarReference>,
    #[serde(default, deserialize_with = "bounded_optional_evidence_vec")]
    occurrences: Option<Vec<SidecarOccurrence>>,
    assembly: String,
    framework: String,
    language: String,
    output_kind: String,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    defines: Vec<String>,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    usings: Vec<String>,
    generators: String,
    input_closure: String,
    errors: u32,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarOccurrence {
    owner: u32,
    start: u32,
    end: u32,
    target: String,
}

fn bounded_optional_evidence_vec<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where D: serde::Deserializer<'de>, T: serde::Deserialize<'de> {
    bounded_evidence_vec(deserializer).map(Some)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarSource {
    path: String,
    length: u64,
    sha256: String,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct SidecarReference {
    name: String,
    length: u64,
    sha256: String,
}

fn bounded_evidence_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 8192 evidence entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|count| count > 8192) {
                return Err(serde::de::Error::custom("evidence entry limit"));
            }
            let mut values = Vec::new();
            while let Some(value) = seq.next_element()? {
                if values.len() == 8192 {
                    return Err(serde::de::Error::custom("evidence entry limit"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Visitor(std::marker::PhantomData))
}

/// Correlates a helper's same-invocation reference claims with the private
/// assembly bytes captured for the compilation. This is not toolchain or
/// process authentication, and cannot by itself admit a selected unit.
pub(super) fn references_match_snapshot(
    bytes: &[u8],
    expected: &[backend_frontend_csharp::legacy::CSharpTargetingReference],
) -> bool {
    if bytes.len() > 2 * 1024 * 1024 || expected.is_empty() || expected.len() > 8192 { return false; }
    let Ok(sidecar) = serde_json::from_slice::<Sidecar>(bytes) else { return false; };
    if sidecar.references.len() != expected.len() { return false; }
    let mut remaining = std::collections::BTreeMap::new();
    for reference in expected {
        if remaining.insert(reference.name.as_str(), reference).is_some() { return false; }
    }
    for reference in &sidecar.references {
        let Some(captured) = remaining.remove(reference.name.as_str()) else { return false; };
        if reference.length != captured.length || reference.sha256 != hex_sha(&captured.sha256) {
            return false;
        }
    }
    remaining.is_empty()
}

struct Frames<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Frames<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        let end = self.position.checked_add(4)?;
        let length =
            u32::from_be_bytes(self.bytes.get(self.position..end)?.try_into().ok()?) as usize;
        let finish = end.checked_add(length)?;
        let value = self.bytes.get(end..finish)?;
        self.position = finish;
        Some(value)
    }
    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.next()?.try_into().ok()
    }
    fn text(&mut self) -> Option<&'a str> {
        std::str::from_utf8(self.next()?).ok()
    }
    fn count(&mut self, maximum: usize) -> Option<usize> {
        let count = u32::from_be_bytes(self.array::<4>()?) as usize;
        (count > 0 && count <= maximum).then_some(count)
    }
}

/// Strictly decodes bounded CSE1 framing and cross-checks source/config sidecars.
/// Caller must still authenticate compiler attempt, image membership, selected
/// publication, toolchain/helper, current M0 source, and invocation provenance.
pub fn decode_csharp_evidence(bytes: &[u8]) -> Option<CSharpEvidenceClaim<'_>> {
    if bytes.len() > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES {
        return None;
    }
    let mut frames = Frames { bytes, position: 0 };
    if frames.next()? != b"CSE1" {
        return None;
    }
    let target = frames.array::<32>()?;
    let input = frames.array::<32>()?;
    if target == [0; 32] || input == [0; 32] {
        return None;
    }
    ContentId::<backend_version::CompilationTargetDomain>::try_from(target).ok()?;
    let profile = LanguageProfile::try_from(frames.array::<2>()?).ok()?;
    if !matches!(profile, LanguageProfile::CSharp(_)) {
        return None;
    }
    let project_path = frames.text()?;
    if project_path.len() > 4096 {
        return None;
    }
    let unit = CompilationUnitKeyV2::CSharpProject {
        project_path: project_path.into(),
    };
    let expected = csharp_config_probe_paths(&unit)?;
    let config_count = frames.count(17)?;
    if config_count != expected.len() {
        return None;
    }
    let mut config = Vec::with_capacity(config_count);
    let mut config_bytes = 0usize;
    for (index, expected_path) in expected.iter().enumerate() {
        let path = frames.text()?;
        if path != expected_path {
            return None;
        }
        let text = match frames.next()? {
            [0] => None,
            [1] => Some(frames.text()?),
            _ => return None,
        };
        if index == 0 && text.is_none() {
            return None;
        }
        config_bytes = config_bytes
            .checked_add(path.len())?
            .checked_add(text.map_or(0, str::len))?;
        if config_bytes > 2 * 1024 * 1024 {
            return None;
        }
        config.push(CSharpEvidenceConfig { path, text });
    }
    let count = frames.count(8192)?;
    let mut sources = Vec::with_capacity(count);
    let mut prior: Option<&str> = None;
    let mut image_ids = std::collections::BTreeSet::new();
    for _ in 0..count {
        let path = frames.text()?;
        if path.len() > 4096
            || super::compiler::PackageSource::validate_path(path).is_err()
            || !super::compiler::unit_source_matches(&unit, path)
            || !path.to_ascii_lowercase().ends_with(".cs")
            || prior.is_some_and(|old| old >= path)
        {
            return None;
        }
        prior = Some(path);
        let source_fact = frames.array::<32>()?;
        ContentId::<SourceFactDomain>::try_from(source_fact).ok()?;
        let byte_length = u32::from_be_bytes(frames.array::<4>()?);
        if byte_length > 16 * 1024 * 1024 {
            return None;
        }
        let input_content = frames.array::<32>()?;
        let source_sha256 = frames.array::<32>()?;
        if input_content == [0; 32] || source_sha256 == [0; 32] {
            return None;
        }
        let image_identity = frames.array::<32>()?;
        backend_semantic::ir::SemanticImageIdentity::try_from(image_identity).ok()?;
        if !image_ids.insert(image_identity) {
            return None;
        }
        let image_length = u32::from_be_bytes(frames.array::<4>()?);
        if image_length == 0 || image_length as usize > super::compiler::MAX_PACKAGE_SEMANTIC_BYTES
        {
            return None;
        }
        let sidecar = frames.next()?;
        if sidecar.is_empty() || sidecar.len() > 2 * 1024 * 1024 {
            return None;
        }
        sources.push(CSharpEvidenceSource {
            path,
            source_fact,
            byte_length,
            input_content,
            source_sha256,
            image_identity,
            image_length,
            sidecar,
        });
    }
    if frames.position != bytes.len() {
        return None;
    }
    let expected_language = match profile {
        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp10) => {
            "CSharp10"
        }
        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp11) => {
            "CSharp11"
        }
        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp12) => {
            "CSharp12"
        }
        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp13) => {
            "CSharp13"
        }
        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14) => {
            "CSharp14"
        }
        _ => return None,
    };
    let config_input = config
        .iter()
        .map(
            |entry| backend_frontend_csharp::legacy::CSharpCapturedConfig {
                path: entry.path,
                text: entry.text,
            },
        )
        .collect::<Vec<_>>();
    let config_json = serde_json::to_vec(&config_input).ok()?;
    let config_hash = format!("{:x}", Sha256::digest(&config_json));
    let source_lookup = sources
        .iter()
        .map(|source| (source.path, source))
        .collect::<std::collections::BTreeMap<_, _>>();
    let is_hex = |value: &str, length: usize| {
        value.len() == length
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    for source in &sources {
        let value: Sidecar = serde_json::from_slice(source.sidecar).ok()?;
        if !matches!((value.format, value.occurrences.is_some()), (1, false) | (2, true))
            || !is_hex(&value.nonce, 32)
            || !is_hex(&value.image_sha256, 64)
            || value.image_length == 0
            || value.image_length > 32 * 1024 * 1024
            || value.source_binding != source.path
            || value.config_sha256 != config_hash
            || value.language != expected_language
            || !matches!(
                value.output_kind.as_str(),
                "ConsoleApplication" | "DynamicallyLinkedLibrary"
            )
            || value.generators != "unknown"
            || value.input_closure != "unknown"
            || value.errors != 0
        {
            return None;
        }
        if [
            &value.assembly,
            &value.framework,
            &value.language,
            &value.output_kind,
        ]
        .iter()
        .any(|text| text.is_empty() || text.len() > 4096)
            || value.defines.len() > 8192
            || value.usings.len() > 8192
            || value
                .defines
                .iter()
                .chain(&value.usings)
                .any(|text| text.len() > 4096)
        {
            return None;
        }
        if let Some(occurrences) = &value.occurrences {
            let mut target_bytes = 0usize;
            for occurrence in occurrences {
                target_bytes = target_bytes.checked_add(occurrence.target.len())?;
                let claim = backend_frontend_csharp::legacy::parse_foreign_source_claim(&occurrence.target)?;
                let target_path = claim.path()?;
                let target = source_lookup.get(target_path.as_str())?;
                if target_bytes > 1024 * 1024 || occurrence.start >= occurrence.end
                    || occurrence.end > source.byte_length || claim.source_sha256 != target.source_sha256
                    || claim.declaration_end > target.byte_length || target.path == source.path {
                    return None;
                }
                // Owner is an untrusted NCAI coordinate until the selected caller image admits it.
                let _native_owner = occurrence.owner;
            }
        }
        if value.sources.len() != sources.len() {
            return None;
        }
        let mut seen = std::collections::BTreeSet::new();
        for observed in &value.sources {
            let expected = source_lookup.get(observed.path.as_str())?;
            if !seen.insert(observed.path.as_str())
                || observed.length != u64::from(expected.byte_length)
                || observed.sha256 != hex_sha(&expected.source_sha256)
            {
                return None;
            }
        }
        if value.references.is_empty() || value.references.len() > 8192 {
            return None;
        }
        let mut names = std::collections::BTreeSet::new();
        let mut reference_bytes = 0u64;
        for reference in &value.references {
            reference_bytes = reference_bytes.checked_add(reference.length)?;
            if reference.name.is_empty()
                || reference.name.len() > 4096
                || !names.insert(reference.name.to_ascii_lowercase())
                || reference.length == 0
                || reference.length > 32 * 1024 * 1024
                || reference_bytes > 256 * 1024 * 1024
                || !is_hex(&reference.sha256, 64)
            {
                return None;
            }
        }
    }
    Some(CSharpEvidenceClaim {
        target,
        input,
        profile,
        project_path,
        config,
        sources,
    })
}
fn hex_sha(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(64);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").ok();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::CSharpVersion;

    #[test]
    fn invocation_references_must_match_captured_bytes() {
        let aggregate = fixture();
        let decoded = decode_csharp_evidence(&aggregate).unwrap();
        let sidecar = decoded.sources[0].sidecar;
        let mut expected = vec![backend_frontend_csharp::legacy::CSharpTargetingReference {
            name: "System.Runtime".to_owned(), length: 1, sha256: [0xbb; 32],
        }];
        assert!(references_match_snapshot(sidecar, &expected));
        expected[0].sha256[0] ^= 1;
        assert!(!references_match_snapshot(sidecar, &expected));
        expected[0].sha256[0] ^= 1;
        expected[0].length += 1;
        assert!(!references_match_snapshot(sidecar, &expected));
        assert!(!references_match_snapshot(sidecar, &[]));
    }

    fn frame(out: &mut Vec<u8>, value: &[u8]) {
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }
    fn fixture() -> Vec<u8> {
        let source = b"class App {}";
        let source_fact = backend_semantic::ir::SourceIdentity::from_bytes(source).unwrap();
        let input_content = backend_compile::VerifiedSourceText::capture(source)
            .unwrap()
            .content()
            .to_bytes();
        let source_sha: [u8; 32] = Sha256::digest(source).into();
        let config = [
            backend_frontend_csharp::legacy::CSharpCapturedConfig {
                path: "App.csproj",
                text: Some("<Project/>"),
            },
            backend_frontend_csharp::legacy::CSharpCapturedConfig {
                path: "Directory.Build.props",
                text: None,
            },
            backend_frontend_csharp::legacy::CSharpCapturedConfig {
                path: "Directory.Build.targets",
                text: None,
            },
        ];
        let config_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&config).unwrap()));
        let sidecar = serde_json::to_vec(&serde_json::json!({
            "format":1,"nonce":"0".repeat(32),"sourceBinding":"App.cs", "imageSha256":"a".repeat(64), "imageLength":123,
            "configSha256":config_hash,"sources":[{"path":"App.cs","length":source.len(),"sha256":hex_sha(&source_sha)}],
            "references":[{"Name":"System.Runtime","Length":1,"Sha256":"b".repeat(64)}],
            "assembly":"App","framework":"net10.0","language":"CSharp14","outputKind":"DynamicallyLinkedLibrary",
            "defines":[],"usings":[],"generators":"unknown","inputClosure":"unknown","errors":0
        })).unwrap();
        let target =
            ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(b"target");
        let image = backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(b"image");
        let mut output = Vec::new();
        frame(&mut output, b"CSE1");
        frame(&mut output, target.as_ref());
        frame(&mut output, &[7; 32]);
        frame(
            &mut output,
            &<[u8; 2]>::from(LanguageProfile::CSharp(CSharpVersion::CSharp14)),
        );
        frame(&mut output, b"App.csproj");
        frame(&mut output, &3u32.to_be_bytes());
        for entry in config {
            frame(&mut output, entry.path.as_bytes());
            match entry.text {
                Some(text) => {
                    frame(&mut output, &[1]);
                    frame(&mut output, text.as_bytes());
                }
                None => frame(&mut output, &[0]),
            }
        }
        frame(&mut output, &1u32.to_be_bytes());
        frame(&mut output, b"App.cs");
        frame(&mut output, source_fact.identity.as_ref());
        frame(&mut output, &source_fact.byte_len.to_be_bytes());
        frame(&mut output, &input_content);
        frame(&mut output, &source_sha);
        frame(&mut output, image.as_ref());
        frame(&mut output, &5u32.to_be_bytes());
        frame(&mut output, &sidecar);
        output
    }

    #[test]
    fn bounded_sidecar_arrays_refuse_excess_entries_before_full_decode() {
        let value = serde_json::json!({
            "format":1,"nonce":"0".repeat(32),"sourceBinding":"App.cs", "imageSha256":"a".repeat(64), "imageLength":1,
            "configSha256":"b".repeat(64),"sources":[],"references":[],
            "assembly":"App","framework":"net10.0","language":"CSharp14","outputKind":"DynamicallyLinkedLibrary",
            "defines":vec!["x";8193],"usings":[],"generators":"unknown","inputClosure":"unknown","errors":0
        });
        assert!(serde_json::from_slice::<Sidecar>(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn strict_decode_rejects_truncation_and_changed_sidecar() {
        let encoded = fixture();
        let claim = decode_csharp_evidence(&encoded).expect("valid bounded claim");
        assert_eq!(claim.project_path, "App.csproj");
        assert_eq!(claim.sources.len(), 1);
        assert!(decode_csharp_evidence(&encoded[..encoded.len() - 1]).is_none());
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_csharp_evidence(&trailing).is_none());
        let mut altered = encoded;
        let last = altered.len() - 2;
        altered[last] = b'!';
        assert!(decode_csharp_evidence(&altered).is_none());
    }
}
