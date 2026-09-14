use backend_semantic::{FacetKind, Read, ReadManifest};
use backend_version::ScopeRoot;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadManifestWire {
    reads: Vec<ReadWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadWire {
    facet: u16,
    scope: String,
    negative: bool,
    range: u64,
}

pub(crate) fn read_manifest_to_wire(manifest: &ReadManifest) -> ReadManifestWire {
    ReadManifestWire {
        reads: manifest
            .shared_reads()
            .iter()
            .copied()
            .map(|read| ReadWire {
                facet: read.facet().tag(),
                scope: crate::encode_id(read.scope_root().as_bytes()),
                negative: read.is_negative(),
                range: read.range_token(),
            })
            .collect(),
    }
}

pub(crate) fn read_manifest_from_wire(value: &ReadManifestWire) -> Result<ReadManifest, String> {
    let reads = value
        .reads
        .iter()
        .map(read_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    ReadManifest::new(reads).map_err(|error| error.to_string())
}

fn read_from_wire(value: &ReadWire) -> Result<Read, String> {
    let facet = FacetKind::ALL
        .into_iter()
        .find(|facet| facet.tag() == value.facet)
        .ok_or_else(|| "read manifest contains an unknown facet".to_owned())?;
    let scope =
        ScopeRoot::from_bytes(crate::decode_id(&value.scope).map_err(|error| error.to_string())?);
    Ok(match (value.negative, value.range) {
        (false, 0) => Read::exact(facet, scope),
        (true, 0) => Read::negative(facet, scope),
        (false, range) => Read::range(facet, scope, range),
        (true, range) => Read::negative_range(facet, scope, range),
    })
}
