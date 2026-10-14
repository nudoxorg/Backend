//! Shared public schema for the typed native source guard and range contract.

use super::SemanticSourceRequest;
use serde_json::{Value, json};

fn object(properties: Value) -> Value {
    let required = properties
        .as_object()
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn bytes(length: usize) -> Value {
    json!({"type":"array","items":{"type":"integer","minimum":0,"maximum":255},"minItems":length,"maxItems":length})
}
fn history_variant(state: &str, fields: Value) -> Value {
    let mut fields = fields.as_object().cloned().unwrap_or_default();
    fields.insert("state".to_owned(), json!({"const":state}));
    object(Value::Object(fields))
}
impl SemanticSourceRequest {
    /// The same closed native argument grammar used by CLI, named MCP tools,
    /// the dispatcher catalog, and source resource requests.
    pub fn json_schema() -> Value {
        let digest = bytes(32);
        let profile = bytes(2);
        let label =
            json!({"type":"string","minLength":1,"maxLength":crate::MAX_PRODUCT_TEXT_BYTES});
        let count = json!({"type":"integer","minimum":0,"maximum":u32::MAX});
        let package = object(json!({"kind":{"enum":["local","purl"]},"value":label}));
        let frontier = object(
            json!({"package":package,"source_relation_root":digest,"source_version":digest,"file_count":count}),
        );
        let freshness = json!({"oneOf":[
            history_variant("current", json!({"input_digest":digest})),
            history_variant("historical", json!({"selected_input":digest,"latest_input":digest})),
            history_variant("unverified", json!({})),
        ]});
        let metadata = object(json!({
            "package":package,"coordinate":label,"profile":profile,"generation":digest,
            "generation_root":digest,"dependency_set":digest,"manifest":digest,
            "artifacts":{"type":"integer","minimum":1,"maximum":u32::MAX},
            "semantic_bytes":count,"complete":{"type":"boolean"},"selected":{"type":"boolean"},
            "freshness":freshness,"selected_source_frontier":{"oneOf":[frontier,{"type":"null"}]},
        }));
        let selection = object(json!({
            "namespace":bytes(16),"profile":profile,"source_coordinate":digest,
            "selection_revision":{"type":"integer","minimum":1},"selected_root":digest,
            "closure_id":digest,"catalog_root":digest,
        }));
        let proof = object(json!({
            "selection":selection,"target_package":label,"target_coordinate":label,
            "package_identity":digest,"image_count":{"type":"integer","minimum":1,"maximum":u32::MAX},
            "reference_tip":digest,"reachable_commit":digest,"input_replay_status":{"const":"unproven"},
        }));
        let selection_id = json!({"selection_id":digest});
        let reason = json!({"selection_id":digest,"reason":{"type":"string","maxLength":crate::surface::MAX_SEMANTIC_HISTORY_STATUS_DETAIL_BYTES}});
        let history = json!({"oneOf":[
            history_variant("not_selected", json!({})),
            history_variant("not_requested", selection_id),history_variant("pending", selection_id),
            history_variant("deferred", reason),history_variant("refused", reason),
            history_variant("superseded", selection_id),
            history_variant("published", json!({"selection_id":digest,"commit":digest,"reference":{"const":"selected-native-v3"},"proof":proof})),
        ]});
        let source = object(json!({
            "schema":{"const":1},"version":digest,
            "total_bytes":{"type":"integer","minimum":1,"maximum":crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES},
        }));
        let reference = object(json!({
            "source":source,"source_relation_root":digest,"semantic_relation_root":digest,
            "metadata":metadata,"history":history,"seal":digest,
        }));
        json!({
            "type":"object","properties":{
                "reference":reference,
                "range":{"type":"object","properties":{
                    "offset":{"type":"integer","minimum":0,"maximum":u32::MAX,"default":0},
                    "max_bytes":{"type":"integer","minimum":1,"maximum":crate::MAX_PRODUCT_TEXT_BYTES,"default":crate::MAX_PRODUCT_TEXT_BYTES},
                },"required":["offset","max_bytes"],"additionalProperties":false},
            },"required":["reference"],"additionalProperties":false,
        })
    }
}

impl crate::SemanticShapeReadRequest {
    /// Shared closed current-shape selector grammar, including legacy inline
    /// source epochs and exact native portable source guards.
    pub fn json_schema() -> Value {
        let source_request = SemanticSourceRequest::json_schema();
        let reference = source_request["properties"]["reference"].clone();
        let mut inline = reference["properties"]["metadata"].clone();
        let mut history = reference["properties"]["history"].clone();
        let image = object(json!({
            "image":object(json!({"artifact_ordinal":{"type":"integer","minimum":0,"maximum":u32::MAX},
                "semantic_generation":bytes(32),"manifest_root":bytes(32),"image_identity":bytes(32)})),
            "history_commit":bytes(32),"parent_commits":{"type":"array","items":bytes(32),"maxItems":1},
        }));
        if let Some(variants) = history["oneOf"].as_array_mut() {
            for variant in variants {
                if variant["properties"]["state"]["const"] == "published" {
                    let proof = &mut variant["properties"]["proof"];
                    proof["properties"]
                        .as_object_mut()
                        .map(|properties| properties.remove("image_count"));
                    proof["properties"]["images"] =
                        json!({"type":"array","items":image,"minItems":1});
                    if let Some(required) = proof["required"].as_array_mut() {
                        required.retain(|name| name != "image_count");
                        required.push(json!("images"));
                    }
                }
            }
        }
        inline["properties"]["history_status"] = history;
        if let Some(required) = inline["required"].as_array_mut() {
            required.push(json!("history_status"));
        }
        let epoch =
            object(json!({"source_relation_root":bytes(32),"semantic_relation_root":bytes(32)}));
        json!({"type":"object","properties":{
            "source":{"oneOf":[inline,reference]},"source_epoch":epoch,
            "symbols":{"type":"array","items":bytes(32),"minItems":1,"maxItems":crate::MAX_SEMANTIC_SHAPE_BATCH},
            "max_nodes":{"type":"integer","minimum":1,"maximum":crate::MAX_SEMANTIC_SHAPE_NODES},
            "max_bytes":{"type":"integer","minimum":1,"maximum":crate::MAX_SEMANTIC_SHAPE_BYTES},
        },"required":["source","symbols","max_nodes","max_bytes"],"additionalProperties":false,
        "allOf":[{"if":{"properties":{"source":{"required":["generation"]}}},"then":{"required":["source_epoch"]}}]})
    }
}
