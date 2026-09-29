// Admission provenance binds the selected-source observation to a commit.
use super::*;
pub(super) fn decode_hex_digest(value: &str) -> Result<[u8; 32], String> {
    let mut output = [0; 32];
    for (index, bytes) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = (bytes[0] as char)
            .to_digit(16)
            .ok_or_else(|| "semantic history filename is malformed".to_owned())?;
        let low = (bytes[1] as char)
            .to_digit(16)
            .ok_or_else(|| "semantic history filename is malformed".to_owned())?;
        output[index] = u8::try_from((high << 4) | low)
            .map_err(|_| "semantic history filename is malformed".to_owned())?;
    }
    Ok(output)
}

pub(super) fn selected_generation_provenance(generation: &LocalSemanticGeneration) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HISTORY_PROVENANCE_DOMAIN);
    hasher.update(generation.selected_stamp.namespace());
    hasher.update(&<[u8; 2]>::from(generation.selected_stamp.profile()));
    hasher.update(generation.selected_stamp.source_coordinate());
    hasher.update(&generation.selected_stamp.selection_revision().to_be_bytes());
    hasher.update(generation.selected_stamp.selected_root());
    hasher.update(generation.selected_stamp.closure_id());
    hasher.update(generation.selected_stamp.catalog_root().as_bytes());
    hasher.update(generation.image_identity.as_ref());
    hasher.update(generation.manifest.root().as_bytes());
    *hasher.finalize().as_bytes()
}
