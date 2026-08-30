// aux-build: serde_json.rs
#![allow(dead_code)]

#[macro_use]
extern crate serde_json;

fn closed_request() -> serde_json::Value {
    json!({
        "filter": {
            "must": [{ "key": "snapshot", "match": { "value": 7 } }]
        }
    })
}

#[allow(
    nudox_dynamic_json_construction,
    reason = "this final plugin extension payload is intentionally open"
)]
fn open_extension() -> serde_json::Value {
    json!({ "plugin_extension": { "unknown": true } })
}

fn main() {}
