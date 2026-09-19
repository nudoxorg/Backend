//! Per-ecosystem metadata grammars. Every function is pure and bounded by its caller.

use serde_json::Value;

use super::{EcosystemAdapter, RegistryChecksum, TransportFailure, component};
use crate::registry::{
    DownloadCount, DownloadCountGap, ReleaseFacts, ReleaseStanding, SecurityStanding,
};

impl EcosystemAdapter {
    pub(super) fn decode_cargo(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut releases = Vec::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let row: Value = serde_json::from_str(line).map_err(|_| TransportFailure::Protocol)?;
            let yanked = row.get("yanked").and_then(Value::as_bool).unwrap_or(false);
            if field(&row, "name")? != self.package.as_str() {
                return Err(TransportFailure::Protocol);
            }
            let version = field(&row, "vers")?;
            let checksum = RegistryChecksum::sha256_hex(field(&row, "cksum")?)?;
            let download_root = if self.endpoint.url() == "https://index.crates.io" {
                "https://static.crates.io"
            } else {
                self.endpoint.url()
            };
            let url = format!(
                "{download_root}/crates/{}/{}-{}.crate",
                component(self.package.as_str()),
                component(self.package.as_str()),
                component(version)
            );
            releases.push(self.release_with_facts(
                version,
                url,
                checksum,
                line.as_bytes(),
                facts(
                    if yanked {
                        ReleaseStanding::Yanked
                    } else {
                        ReleaseStanding::Available
                    },
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                ),
            )?);
        }
        Ok(releases)
    }

    pub(super) fn decode_npm(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let versions = root
            .get("versions")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        versions
            .iter()
            .map(|(version, row)| {
                let dist = row.get("dist").ok_or(TransportFailure::Protocol)?;
                let integrity = field(dist, "integrity")?
                    .strip_prefix("sha512-")
                    .ok_or(TransportFailure::Protocol)?;
                let checksum = RegistryChecksum::sha512_base64(integrity)?;
                let encoded = serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?;
                let standing = row
                    .get("deprecated")
                    .and_then(Value::as_str)
                    .filter(|message| !message.is_empty())
                    .map_or(ReleaseStanding::Available, |_| ReleaseStanding::Deprecated);
                let downloads = row.get("downloads").and_then(Value::as_u64).map_or(
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    DownloadCount::Exact,
                );
                self.release_with_facts(
                    version,
                    field(dist, "tarball")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(standing, downloads),
                )
            })
            .collect()
    }

    pub(super) fn decode_python(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let releases = root
            .get("releases")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        releases
            .iter()
            .map(|(version, files)| {
                let file = files
                    .as_array()
                    .ok_or(TransportFailure::Protocol)?
                    .iter()
                    .filter(|row| row.get("packagetype").and_then(Value::as_str) == Some("sdist"))
                    .min_by_key(|row| row.get("filename").and_then(Value::as_str).unwrap_or(""))
                    .ok_or(TransportFailure::Protocol)?;
                let digest = field(
                    file.get("digests").ok_or(TransportFailure::Protocol)?,
                    "sha256",
                )?;
                let checksum = RegistryChecksum::sha256_hex(digest)?;
                let encoded = serde_json::to_vec(file).map_err(|_| TransportFailure::Protocol)?;
                let yanked = match file.get("yanked") {
                    Some(Value::Bool(value)) => *value,
                    Some(Value::String(reason)) => !reason.is_empty(),
                    None => false,
                    Some(_) => return Err(TransportFailure::Protocol),
                };
                let standing = if yanked {
                    ReleaseStanding::Yanked
                } else {
                    ReleaseStanding::Available
                };
                self.release_with_facts(
                    version,
                    field(file, "url")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(
                        standing,
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )
            })
            .collect()
    }

    pub(super) fn decode_maven(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        tagged_rows(text, "release")
            .map(|attributes| {
                let version = attribute(attributes, "version")?;
                let checksum = RegistryChecksum::sha256_hex(attribute(attributes, "sha256")?)?;
                self.release(
                    version,
                    attribute(attributes, "url")?.to_owned(),
                    checksum,
                    attributes.as_bytes(),
                )
            })
            .collect()
    }

    pub(super) fn decode_nuget(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let rows = root
            .get("items")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        rows.iter()
            .map(|row| {
                let entry = row.get("catalogEntry").ok_or(TransportFailure::Protocol)?;
                let version = field(entry, "version")?;
                let checksum = RegistryChecksum::sha512_base64(field(row, "packageHash")?)?;
                let encoded = serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?;
                let standing = row
                    .get("listed")
                    .and_then(Value::as_bool)
                    .is_some_and(|listed| !listed)
                    .then_some(ReleaseStanding::Unlisted)
                    .unwrap_or(ReleaseStanding::Available);
                let downloads = row.get("totalDownloads").and_then(Value::as_u64).map_or(
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    DownloadCount::Exact,
                );
                self.release_with_facts(
                    version,
                    field(row, "packageContent")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(standing, downloads),
                )
            })
            .collect()
    }

    pub(super) fn decode_go(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut fields = line.split_ascii_whitespace();
                let version = fields.next().ok_or(TransportFailure::Protocol)?;
                let digest = fields.next().ok_or(TransportFailure::Protocol)?;
                if fields.next().is_some() {
                    return Err(TransportFailure::Protocol);
                }
                let checksum = RegistryChecksum::sha256_hex(digest)?;
                let module = self.go_proxy_package();
                let url = format!(
                    "{}/{}/@v/{}.zip",
                    self.endpoint.url(),
                    module,
                    component(version)
                );
                self.release(version, url, checksum, line.as_bytes())
            })
            .collect()
    }

    pub(super) fn decode_cpp(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let rows = root
            .get("results")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        rows.iter()
            .map(|row| {
                let version = field(row, "version")?;
                let checksum = RegistryChecksum::sha256_hex(field(row, "sha256")?)?;
                let encoded = serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?;
                self.release(
                    version,
                    field(row, "download_url")?.to_owned(),
                    checksum,
                    &encoded,
                )
            })
            .collect()
    }
}

fn facts(standing: ReleaseStanding, downloads: DownloadCount) -> ReleaseFacts {
    ReleaseFacts::new(standing, downloads, SecurityStanding::Unassessed)
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, TransportFailure> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or(TransportFailure::Protocol)
}
fn tagged_rows<'a>(text: &'a str, tag: &'a str) -> impl Iterator<Item = &'a str> {
    // `split('<')` has already consumed the opening delimiter.  Matching it
    // again made every valid `<release .../>` row invisible and caused the
    // Maven native feed to normalize as an empty page.
    let prefix = format!("{tag} ");
    text.split('<').filter_map(move |part| {
        part.strip_prefix(&prefix)
            .and_then(|row| row.split_once("/>").map(|(attributes, _)| attributes))
    })
}
fn attribute<'a>(row: &'a str, name: &str) -> Result<&'a str, TransportFailure> {
    let needle = format!("{name}=\"");
    let start = row.find(&needle).ok_or(TransportFailure::Protocol)? + needle.len();
    let suffix = &row[start..];
    let end = suffix.find('"').ok_or(TransportFailure::Protocol)?;
    Ok(&suffix[..end])
}
