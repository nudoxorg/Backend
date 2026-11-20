//! Bounded streaming decoder for MSBuild's projected reference JSON.
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::fmt;

pub(super) struct Projection {
    pub sdk_path: String,
    pub items: Vec<ProjectedReference>,
}
pub(super) struct ProjectedReference {
    pub path: String,
    pub fusion: String,
}

struct BoundedString(usize);
impl<'de> DeserializeSeed<'de> for BoundedString {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        struct V(usize);
        impl<'de> Visitor<'de> for V {
            type Value = String;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("bounded string") }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<String, E> {
                if s.len() > self.0 { return Err(E::custom("string limit")); }
                Ok(s.to_owned())
            }
        }
        d.deserialize_str(V(self.0))
    }
}
struct Reject;
impl<'de> DeserializeSeed<'de> for Reject {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
        Err(de::Error::custom("reference count limit"))
    }
}
struct Reference;
impl<'de> DeserializeSeed<'de> for Reference {
    type Value = ProjectedReference;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ProjectedReference;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("reference object") }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut identity, mut full, mut fusion) = (None, None, None);
                while let Some(key) = map.next_key_seed(BoundedString(256))? {
                    let slot = match key.as_str() {
                        "Identity" => Some(&mut identity), "FullPath" => Some(&mut full),
                        "FusionName" => Some(&mut fusion), _ => None,
                    };
                    if let Some(slot) = slot {
                        if slot.is_some() { return Err(de::Error::custom("duplicate reference field")); }
                        *slot = Some(map.next_value_seed(BoundedString(4096))?);
                    } else { map.next_value::<IgnoredAny>()?; }
                }
                let path: String = full.or(identity).ok_or_else(|| de::Error::custom("reference path missing"))?;
                let fusion: String = fusion.ok_or_else(|| de::Error::custom("fusion missing"))?;
                if path.is_empty() || fusion.is_empty() { return Err(de::Error::custom("empty reference field")); }
                Ok(ProjectedReference { path, fusion })
            }
        }
        d.deserialize_map(V)
    }
}
struct References(usize);
impl<'de> DeserializeSeed<'de> for References {
    type Value = Vec<ProjectedReference>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct V(usize);
        impl<'de> Visitor<'de> for V {
            type Value = Vec<ProjectedReference>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("bounded reference array") }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut items = Vec::new();
                while items.len() < self.0 {
                    match seq.next_element_seed(Reference)? { Some(item) => items.push(item), None => return Ok(items) }
                }
                // The seed rejects before the deserializer descends into a surplus item.
                if seq.next_element_seed(Reject)?.is_some() { return Err(de::Error::custom("reference count limit")); }
                Ok(items)
            }
        }
        d.deserialize_seq(V(self.0))
    }
}
struct NamedObject { field: &'static str, limit: usize, references: bool }
enum Named { Path(String), Items(Vec<ProjectedReference>) }
impl<'de> DeserializeSeed<'de> for NamedObject {
    type Value = Named;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct V(NamedObject);
        impl<'de> Visitor<'de> for V {
            type Value = Named;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SDK result object") }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut value = None;
                while let Some(key) = map.next_key_seed(BoundedString(256))? {
                    if key == self.0.field {
                        if value.is_some() { return Err(de::Error::custom("duplicate SDK field")); }
                        value = Some(if self.0.references { Named::Items(map.next_value_seed(References(self.0.limit))?) }
                            else { Named::Path(map.next_value_seed(BoundedString(4096))?) });
                    } else { map.next_value::<IgnoredAny>()?; }
                }
                value.ok_or_else(|| de::Error::custom("SDK field missing"))
            }
        }
        d.deserialize_map(V(self))
    }
}
struct Root(usize);
impl<'de> DeserializeSeed<'de> for Root {
    type Value = Projection;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct V(usize);
        impl<'de> Visitor<'de> for V {
            type Value = Projection;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SDK projection") }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let (mut sdk_path, mut items) = (None, None);
                while let Some(key) = map.next_key_seed(BoundedString(256))? {
                    match key.as_str() {
                        "Properties" => {
                            if sdk_path.is_some() { return Err(de::Error::custom("duplicate properties")); }
                            let Named::Path(path) = map.next_value_seed(NamedObject { field: "MSBuildSDKsPath", limit: 0, references: false })? else { return Err(de::Error::custom("invalid path")); };
                            sdk_path = Some(path);
                        }
                        "Items" => {
                            if items.is_some() { return Err(de::Error::custom("duplicate items")); }
                            let Named::Items(list) = map.next_value_seed(NamedObject { field: "NudoxReference", limit: self.0, references: true })? else { return Err(de::Error::custom("invalid items")); };
                            items = Some(list);
                        }
                        _ => { map.next_value::<IgnoredAny>()?; }
                    }
                }
                let sdk_path: String = sdk_path.ok_or_else(|| de::Error::custom("SDK path missing"))?;
                let items: Vec<ProjectedReference> = items.ok_or_else(|| de::Error::custom("SDK references missing"))?;
                if sdk_path.is_empty() || items.is_empty() { return Err(de::Error::custom("empty SDK result")); }
                Ok(Projection { sdk_path, items })
            }
        }
        d.deserialize_map(V(self.0))
    }
}
pub(super) fn decode(bytes: &[u8], limit: usize) -> Result<Projection, &'static str> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let result = Root(limit).deserialize(&mut deserializer).map_err(|_| "SDK resolution malformed")?;
    deserializer.end().map_err(|_| "SDK resolution malformed")?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_sdk_output_retains_required_identity_only() {
        // Shape of MSBuild -getItem output, including intrinsic item metadata.
        let output = br#"{"Properties":{"MSBuildSDKsPath":"/sdk/Sdks"},"Items":{"NudoxReference":[{"Identity":"/packs/A.dll","FullPath":"/packs/A.dll","FusionName":"A, Version=1.0.0.0","Filename":"A","Extension":".dll"}]}}"#;
        let result = decode(output, 1).unwrap();
        assert_eq!(result.sdk_path, "/sdk/Sdks");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].path, "/packs/A.dll");
        assert_eq!(result.items[0].fusion, "A, Version=1.0.0.0");
    }

    #[test]
    fn surplus_reference_is_rejected_before_descending() {
        let output = br#"{"Properties":{"MSBuildSDKsPath":"/sdk"},"Items":{"NudoxReference":[{"FullPath":"/a.dll","FusionName":"A"},{"large_unparsed":["#;
        let mut deserializer = serde_json::Deserializer::from_slice(output);
        let error = match Root(1).deserialize(&mut deserializer) {
            Ok(_) => panic!("surplus reference accepted"), Err(error) => error,
        };
        assert!(error.to_string().contains("reference count limit"));
    }

    #[test]
    fn dense_unused_metadata_is_skipped_and_selected_fields_are_bounded() {
        let dense = (0..20000).map(|_| "0").collect::<Vec<_>>().join(",");
        let output = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{{"FullPath":"/a.dll","FusionName":"A","unused":[{dense}]}}]}}}}"#);
        assert_eq!(decode(output.as_bytes(), 1).unwrap().items.len(), 1);
        let oversized = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{{"FullPath":"/a.dll","FusionName":"{}"}}]}}}}"#, "x".repeat(4097));
        assert!(decode(oversized.as_bytes(), 1).is_err());
    }
    #[test]
    fn allocations_reject_surplus_before_descending_into_large_item() {
        let payload = "x".repeat(1_000_000);
        let input = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{{"FullPath":"/a.dll","FusionName":"A"}},{{"surplus":"{payload}"}}]}}}}"#);
        let mut rejected = false;
        let allocations = allocation_counter::measure(|| {
            rejected = std::hint::black_box(decode(input.as_bytes(), 1)).is_err();
        });
        assert!(rejected);
        assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
        assert!(allocations.bytes_max < 16 * 1024, "{allocations:?}");
    }

    #[test]
    fn allocations_skip_dense_metadata_without_materializing_it() {
        let dense = "0,".repeat(300_000) + "0";
        let input = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{{"FullPath":"/a.dll","FusionName":"A","unused":[{dense}]}}]}}}}"#);
        let mut accepted = false;
        let allocations = allocation_counter::measure(|| {
            accepted = std::hint::black_box(decode(input.as_bytes(), 1)).is_ok();
        });
        assert!(accepted);
        assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
        assert!(allocations.bytes_max < 16 * 1024, "{allocations:?}");
    }

    #[test]
    fn allocations_bound_escaped_scratch_and_selected_vector_growth() {
        let escaped = r"\u0078".repeat(4097);
        let invalid = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{{"FullPath":"/a.dll","FusionName":"{escaped}"}}]}}}}"#);
        let mut rejected = false;
        let allocations = allocation_counter::measure(|| {
            rejected = std::hint::black_box(decode(invalid.as_bytes(), 1)).is_err();
        });
        assert!(rejected);
        // serde_json may allocate unescape scratch before our string visitor.
        assert!(allocations.bytes_total <= (invalid.len() * 3 + 8192) as u64, "{allocations:?}");
        assert!(allocations.bytes_max <= (invalid.len() * 2 + 8192) as u64, "{allocations:?}");
        let item = r#"{"FullPath":"/a.dll","FusionName":"A"}"#;
        let references = vec![item; 64].join(",");
        let valid = format!(r#"{{"Properties":{{"MSBuildSDKsPath":"/sdk"}},"Items":{{"NudoxReference":[{references}]}}}}"#);
        let mut count = 0;
        let allocations = allocation_counter::measure(|| {
            count = std::hint::black_box(decode(valid.as_bytes(), 64).unwrap()).items.len();
        });
        assert_eq!(count, 64);
        assert!(allocations.bytes_total <= (valid.len() * 8 + 8192) as u64, "{allocations:?}");
        assert!(allocations.bytes_max <= (valid.len() * 4 + 8192) as u64, "{allocations:?}");
    }

}
