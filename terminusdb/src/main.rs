use serde_json::json;

fn main() {
    let example = ir::entry::Entry {
        name: "testing".to_string(),
        id: 120910490124i64,
        path: vec![
            "jsonld".to_string(),
            "convert".to_string(),
            "testing".to_string(),
        ],
        kind: ir::kind::Kind::Function(ir::function::Function {
            input_parameters: None,
            output_parameters: None,
            attributes: None,
            generics: None,
            name: String::from("fnfunction"),
            implemented: true,
            visibility: Some(ir::kind::Visibility::Public),
        }),
        visibility: Some(ir::kind::Visibility::Public),
        documentation: Some(String::from("This is the documentation for this item here")),
        members: vec![ir::entry::EntryRef {
            id: 1234,
            path: vec![
                "jsonld".to_string(),
                "generate".to_string(),
                "testing".to_string(),
            ],
        }],
        input_parameters: None,
        output_parameters: None,
        type_parameters: None,
    };

    let out = example.to_docs();
    dbg!(&out);

    let out = serde_json::ser::to_string_pretty(&out).unwrap();
    print!("{}", out);
}

trait ToTerminusDB {
    fn to_docs(&self) -> Vec<serde_json::Value>;
}

fn kind_payload_id(entry_id: &str, tag: &str) -> String {
    format!("{tag}/{entry_id}")
}

/// Outputs the Documents fo each entry
/// Keeps surface level, no deep nesting in the documents
impl ToTerminusDB for ir::entry::Entry {
    fn to_docs(&self) -> Vec<serde_json::Value> {
        let fq_name = self.path.join("::");
        let fq_id = self.path.join("/");

        let vis = self.visibility.as_ref().unwrap();
        let members: Vec<serde_json::Value> = self
            .members
            .iter()
            .map(|entry_ref| {
                let fq_id = entry_ref.path.join("/");
                let id = format!("Entry/{}", fq_id);
                json!({"@id": id})
            })
            .collect();
        let entry_id = format!("Entry/{}", fq_id);
        // hold documents... Remember each document will have multple nested documents
        let mut out = vec![];
        let kind_tag: &'static str = match &self.kind {
            ir::kind::Kind::Module => "Module",
            ir::kind::Kind::RecordType(_) => "RecordType",
            ir::kind::Kind::Info => "Info",
            ir::kind::Kind::UnionType(_) => "UnionType",
            ir::kind::Kind::TraitDef(_) => "TraitDef",
            ir::kind::Kind::TraitImpl(_) => "TraitImpl",
            ir::kind::Kind::SumType(_) => "SumType",
            ir::kind::Kind::InterfaceType => "InterfaceType",
            ir::kind::Kind::Function(_) => "Function",
            ir::kind::Kind::TypeAlias(_) => "TypeAlias",
            ir::kind::Kind::Constant => "Constant",
            ir::kind::Kind::Variable => "Variable",
            ir::kind::Kind::Macro => "Macro",
            ir::kind::Kind::PrimitiveType => "PrimitiveType",
            ir::kind::Kind::Field => "Field",
            ir::kind::Kind::Event => "Event",
        };

        let kind_payload_id = kind_payload_id(&entry_id, &kind_tag);
        let kind_id = json!({"@id": kind_payload_id});
        out.push(json!({
            "@type": "Entry",
            "@id": entry_id,
            "id": self.id,
            "path": self.path,
            "fq_name": fq_name,
            "members": members,
            "kind_tag": kind_tag,
            "kind": kind_id,
            "visibility": vis,
        }));

        let kind_blob = &self.kind;
        // (TODO-add Kind to output values.. use the kind_id as top level node..)
        out.push(json!({
            "@id": kind_payload_id,
            "data": kind_blob
        }));

        out
    }
}
