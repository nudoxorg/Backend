//! Canonical feed parsing and source-policy admission, separate from network I/O.

use std::sync::Arc;

use serde_json::Value;

use super::identity::coordinate_from_registry_parts;
use super::transport::ArchiveIntegrity;
use super::{
    CanonicalFeedV1, FeedPage, FeedRequest, FeedSchema, PackageName, PackageVersion,
    ProvenanceDigest, RegistryEcosystem, RemotePackage, TransportFailure,
};

pub(super) fn admit_page(
    bytes: &[u8],
    request: FeedRequest,
    ecosystem: RegistryEcosystem,
    endpoint: &str,
) -> Result<FeedPage, TransportFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
    let object = value.as_object().ok_or(TransportFailure::Protocol)?;
    admit_envelope(object)?;
    let next = parse_digest(text(object, "next")?)?;
    let rows = object
        .get("items")
        .and_then(Value::as_array)
        .ok_or(TransportFailure::Protocol)?;
    if rows.len() > request.max_items {
        return Err(TransportFailure::Overrun {
            measured: u64::try_from(rows.len()).map_err(|_| TransportFailure::Bounds)?,
            limit: u64::try_from(request.max_items).map_err(|_| TransportFailure::Bounds)?,
        });
    }
    let mut packages = Vec::with_capacity(rows.len());
    for row in rows {
        packages.push(admit_package(row, ecosystem, endpoint)?);
    }
    if packages
        .windows(2)
        .any(|pair| pair[0].coordinate >= pair[1].coordinate)
    {
        return Err(TransportFailure::Protocol);
    }
    Ok(FeedPage {
        base: request.cursor,
        next_token: next,
        packages,
    })
}

fn admit_envelope(object: &serde_json::Map<String, Value>) -> Result<(), TransportFailure> {
    if object.len() == 3
        && object.get("schema").and_then(Value::as_u64) == Some(u64::from(CanonicalFeedV1::VERSION))
    {
        Ok(())
    } else {
        Err(TransportFailure::Protocol)
    }
}

fn admit_package(
    value: &Value,
    ecosystem: RegistryEcosystem,
    endpoint: &str,
) -> Result<RemotePackage, TransportFailure> {
    let row = value.as_object().ok_or(TransportFailure::Protocol)?;
    if row.len() != 5 {
        return Err(TransportFailure::Protocol);
    }
    let name = PackageName::new(text(row, "name")?).map_err(|_| TransportFailure::Protocol)?;
    let version =
        PackageVersion::new(text(row, "version")?).map_err(|_| TransportFailure::Protocol)?;
    Ok(RemotePackage {
        coordinate: coordinate_from_registry_parts(ecosystem, name.as_str(), version.as_str())
            .map_err(|_| TransportFailure::Protocol)?,
        integrity: ArchiveIntegrity::Canonical(parse_digest(text(row, "blake3")?)?),
        provenance: ProvenanceDigest::from_authenticated_feed(parse_digest(text(
            row,
            "provenance",
        )?)?),
        archive_url: Arc::from(admit_archive_url(endpoint, text(row, "archive")?)?),
    })
}

fn text<'a>(
    row: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a str, TransportFailure> {
    row.get(name)
        .and_then(Value::as_str)
        .ok_or(TransportFailure::Protocol)
}

fn admit_archive_url(endpoint: &str, value: &str) -> Result<String, TransportFailure> {
    let source = origin(endpoint)?;
    if value.starts_with('/') {
        return Ok(format!("{source}{value}"));
    }
    value
        .starts_with(&format!("{source}/"))
        .then(|| value.to_owned())
        .ok_or(TransportFailure::Configuration)
}

fn origin(endpoint: &str) -> Result<&str, TransportFailure> {
    let scheme = endpoint
        .find("://")
        .ok_or(TransportFailure::Configuration)?
        + 3;
    Ok(endpoint[scheme..]
        .find('/')
        .map_or(endpoint, |relative| &endpoint[..scheme + relative]))
}

fn parse_digest(value: &str) -> Result<[u8; 32], TransportFailure> {
    if value.len() != 64 {
        return Err(TransportFailure::Protocol);
    }
    let mut output = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(output)
}

fn nibble(value: u8) -> Result<u8, TransportFailure> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(TransportFailure::Protocol),
    }
}
