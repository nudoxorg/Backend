//! Protocol golden and falsifier tests for the C# adapter.
//! Each test names one rejection or fidelity law.
//! No live dotnet process is required for these tests.

use backend_frontend_csharp::legacy::{
    DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet_path,
};

const GOLDEN: &[u8] = include_bytes!("fixtures/golden.json");

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Tooling(#[from] ToolingUnavailable),
    #[error("the deliberately bogus tool unexpectedly started: {0:?}")]
    UnexpectedTool(std::path::PathBuf),
}

#[test]
fn golden_preserves_three_nullability_cells_and_docs() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let ty = &output.types[0];
    assert_eq!(
        ty.doc_links.as_ref().and_then(|links| links.get("T")),
        Some(&"T".to_owned())
    );
    let fields = &ty.members.fields;
    assert!(matches!(
        fields[0].r#type,
        Type::Named {
            nullable: Nullability::Annotated,
            ..
        }
    ));
    assert!(matches!(
        fields[1].r#type,
        Type::Named {
            nullable: Nullability::Oblivious,
            ..
        }
    ));
    assert!(matches!(
        fields[2].r#type,
        Type::Named {
            nullable: Nullability::NotAnnotated,
            ..
        }
    ));
    Ok(())
}

#[test]
fn golden_named_nodes_keep_owner_and_type_kind() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let base = output.types[0].base_type.as_ref().expect("baseType");
    match base {
        Type::Named {
            name,
            args,
            owner,
            nullable,
            type_kind,
        } => {
            assert_eq!(name, "System.Object");
            assert!(args.is_empty());
            assert!(owner.is_none());
            assert_eq!(*nullable, Nullability::NotAnnotated);
            assert_eq!(type_kind, "Class");
        }
        other => panic!("expected a named base type, got {other:?}"),
    }
    let iface = &output.types[0].interfaces[0];
    match iface {
        Type::Named { owner, .. } => assert!(owner.is_none()),
        other => panic!("expected a named interface, got {other:?}"),
    }
    Ok(())
}

#[test]
fn golden_decodes_pointer_nullable_value_tuple_and_func_ptr() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let fields = &output.types[0].members.fields;
    match &fields[3].r#type {
        Type::Pointer { pointee } => match pointee.as_ref() {
            Type::Named {
                name, type_kind, ..
            } => {
                assert_eq!(name, "System.Int32");
                assert_eq!(type_kind, "Struct");
            }
            other => panic!("expected a named pointee, got {other:?}"),
        },
        other => panic!("expected a pointer field, got {other:?}"),
    }
    match &fields[4].r#type {
        Type::NullableValue { inner } => assert!(matches!(
            inner.as_ref(),
            Type::Named { name, .. } if name == "System.Int32"
        )),
        other => panic!("expected a nullableValue field, got {other:?}"),
    }
    match &fields[5].r#type {
        Type::Tuple {
            elements, nullable, ..
        } => {
            assert_eq!(elements.len(), 2);
            assert_eq!(elements[0].name.as_deref(), Some("first"));
            assert!(elements[1].name.is_none());
            assert_eq!(*nullable, Nullability::Oblivious);
        }
        other => panic!("expected a tuple field, got {other:?}"),
    }
    match &fields[6].r#type {
        Type::FunctionPointer {
            params,
            return_type,
            call_conv,
            unmanaged_call_convs,
        } => {
            assert_eq!(params.len(), 1);
            assert!(return_type.is_none());
            assert_eq!(call_conv, "managed");
            assert!(unmanaged_call_convs.is_empty());
        }
        other => panic!("expected a managed funcPtr field, got {other:?}"),
    }
    match &fields[7].r#type {
        Type::FunctionPointer {
            return_type,
            call_conv,
            unmanaged_call_convs,
            ..
        } => {
            assert!(return_type.is_some());
            assert_eq!(call_conv, "unmanaged");
            assert_eq!(unmanaged_call_convs.as_ref(), ["Cdecl"]);
        }
        other => panic!("expected an unmanaged funcPtr field, got {other:?}"),
    }
    assert!(matches!(fields[8].r#type, Type::Dynamic));
    assert!(matches!(
        fields[9].r#type,
        Type::Error { ref name } if name == "Missing"
    ));
    Ok(())
}

#[test]
fn inline_transcript_decodes_named_root_and_function_pointer_shapes() -> Result<(), TestError> {
    let named = br#"{"kind":"named","name":"System.String","args":[],"owner":null,"nullable":"annotated","type_kind":"ReferenceType"}"#;
    assert!(matches!(
        serde_json::from_slice::<Type>(named),
        Ok(Type::Named { name, owner: None, type_kind, .. })
            if name == "System.String" && type_kind == "ReferenceType"
    ));

    let function_pointer = br#"{"kind":"funcPtr","params":[{"kind":"named","name":"System.Int32","args":[],"owner":null,"nullable":"none","type_kind":"Struct"}],"return":null,"call_conv":"unmanaged","unmanaged_call_convs":["Cdecl"]}"#;
    assert!(matches!(
        serde_json::from_slice::<Type>(function_pointer),
        Ok(Type::FunctionPointer { params, return_type: None, call_conv, unmanaged_call_convs })
            if params.len() == 1 && call_conv == "unmanaged" && unmanaged_call_convs.as_ref() == ["Cdecl"]
    ));

    let root = br#"{"format":1,"dotnetVersion":"x","roslyn":"x","mode":"source","assembly":{"name":"Demo","version":"1.0.0.0","tfm":"net10.0","forwardedTypes":[],"ivt":[]},"namespaces":[],"types":[],"references":[],"diagnostics":{"errorTypeCount":0,"errorCount":0,"generatorSupport":"applied"}}"#;
    assert!(
        decode(root).is_ok(),
        "a producer transcript with assembly must decode"
    );
    Ok(())
}

#[test]
fn exception_documentation_is_a_typed_throw_not_an_output_parameter() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    let method = &output.types[0].members.methods[0];
    assert!(matches!(
        method.throws.first(),
        Some(Type::Named { name, .. }) if name == "System.Exception"
    ));
    assert!(
        method
            .parameters
            .iter()
            .all(|parameter| parameter.name != "output")
    );
    Ok(())
}

#[test]
fn golden_retains_every_declaration_and_member_field() -> Result<(), TestError> {
    let output = decode(GOLDEN)?;
    assert_eq!(output.assembly.name, "Demo");
    assert_eq!(output.assembly.tfm, "net10.0");
    assert_eq!(output.namespaces[0].name, "Demo");
    assert_eq!(output.references[0].target, "P:Demo.Widget`1.Name");
    assert_eq!(output.diagnostics.error_type_count, 1);
    assert_eq!(output.diagnostics.generator_support, "applied");

    let ty = &output.types[0];
    assert_eq!(ty.modifiers.as_ref(), ["public"]);
    assert_eq!(ty.type_params[0].variance, "out");
    let constraints = &ty.type_params[0].constraints;
    assert!(constraints.reference_type);
    assert!(!constraints.unmanaged);
    assert!(!constraints.allows_ref_like);
    assert_eq!(
        ty.deprecated.as_ref().and_then(|d| d.message.as_deref()),
        Some("use NewWidget")
    );
    assert_eq!(ty.attributes[0].named.get("Version"), Some(&"3".to_owned()));
    let location = ty.location.as_ref().expect("declaration location");
    assert_eq!(location.file, "Widget.cs");
    assert_eq!(location.end_line, 1);
    assert!(ty.extension_receiver.is_none());

    let members = &ty.members;
    assert_eq!(
        members.fields[0].accessibility, "public",
        "field accessibility retained"
    );
    let color = &output.types[1];
    assert!(color.members.fields[0].is_const);
    assert_eq!(
        color.members.fields[0].constant.as_deref(),
        Some("Red"),
        "enum constant value retained"
    );
    assert_eq!(
        color.enum_underlying.as_ref(),
        Some(&Type::Named {
            name: "System.Int32".into(),
            args: Box::from([]),
            owner: None,
            nullable: Nullability::Oblivious,
            type_kind: "Struct".into(),
        }),
        "enum underlying type retained"
    );

    let method = &members.methods[0];
    assert_eq!(method.method_kind, "Ordinary");
    assert!(method.is_virtual);
    assert!(method.type_params[0].constraints.constructor);
    assert_eq!(method.parameters[0].ref_kind, "ref");
    assert_eq!(members.conversions[0].operator_kind, "implicit");
    assert!(members.indexers[0].is_indexer);
    assert_eq!(members.constructors[0].return_type, None);
    assert_eq!(members.nested.as_ref(), ["T:Demo.Widget`1.Cell"]);
    assert_eq!(
        members.events[0].add_accessibility.as_deref(),
        Some("public")
    );
    assert_eq!(
        members.properties[0].set_accessibility.as_deref(),
        Some("protected")
    );
    Ok(())
}

#[test]
fn mutations_retain_exact_protocol_fault_and_source() -> Result<(), TestError> {
    let unknown =
        br#"{"format":1,"dotnetVersion":"x","roslyn":"x","mode":"source","assembly":{"name":"a","version":"1","tfm":"t","forwardedTypes":[],"ivt":[]},"namespaces":[],"types":[],"references":[],"diagnostics":{"errorTypeCount":0,"errorCount":0,"generatorSupport":"applied"},"surprise":1}"#;
    let stale = br#"{"format":99,"dotnetVersion":"x","roslyn":"x","mode":"source","types":[]}"#;
    let u64_stale =
        br#"{"format":4294967297,"dotnetVersion":"x","roslyn":"x","mode":"source","types":[]}"#;
    let kind = br#"{"format":1,"dotnetVersion":"x","roslyn":"x","mode":"source","types":[{"kind":"alien"}]}"#;
    let truncated = br#"{"format":1,"dotnetVersion":"x","roslyn":"x","mode":"source","types":["#;
    assert!(
        matches!(decode(unknown), Err(DecodeError::UnknownField { ref field, .. }) if field == "surprise")
    );
    assert!(matches!(
        decode(stale),
        Err(DecodeError::StaleSchema { found: 99, .. })
    ));
    // A 2^32+1 schema cell must not fold onto 1 through a u32 cast.
    assert!(matches!(
        decode(u64_stale),
        Err(DecodeError::StaleSchema {
            found: 4294967297,
            ..
        })
    ));
    assert!(
        matches!(decode(kind), Err(DecodeError::UnknownTypeKind { ref kind, .. }) if kind == "alien")
    );
    assert!(matches!(
        decode(truncated),
        Err(DecodeError::TruncatedRecord { .. })
    ));
    Ok(())
}

#[test]
fn bogus_override_is_typed_unavailable_before_work() -> Result<(), TestError> {
    let error = match probe_dotnet_path("/definitely/not-a-dotnet".into()) {
        Err(error) => error,
        Ok(path) => return Err(TestError::UnexpectedTool(path)),
    };
    assert_eq!(error.language, "csharp");
    assert_eq!(error.tool.to_string_lossy(), "/definitely/not-a-dotnet");
    Ok(())
}
