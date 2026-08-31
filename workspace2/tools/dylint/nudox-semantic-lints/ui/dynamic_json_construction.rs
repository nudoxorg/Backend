#![feature(rustc_private)]
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

fn main() {}
