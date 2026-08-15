//! Round-trip and strict-parser tests for the NdIrF1 format.
use super::*;
use crate::wire::{
    AutoFact, AutoState, AutoTrait, EntryPayloadFlags, FnSigFlags, GenericParamWire, ImplFlags,
    ImplWire, ModuleWire, OwnedEntryPayload, ReexportWire, SelfKind, StaticWire, SymbolWire,
    TraitFlags, TraitWire, TypeAliasWire, TypeRefWire, TypeWire, VariantForm, VariantWire,
    WherePredWire,
};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn sref(eco: &str, pkg: &str, n: u8) -> StableRef {
    StableRef::new(
        PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
        intro(n),
    )
}

fn base_sym(name: &str) -> SymbolWire {
    SymbolWire {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: None,
        source_path: String::new(),
        span_start: 0,
        span_end: 1,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    }
}

fn make_payload(sym: SymbolWire, kind: KindWire) -> OwnedEntryPayload {
    let disc = match &kind {
        KindWire::Module(_) => KindDiscriminant::Module,
        KindWire::Record(_) => KindDiscriminant::Record,
        KindWire::Field(_) => KindDiscriminant::Field,
        KindWire::Function(_) => KindDiscriminant::Function,
        KindWire::Type(_) => KindDiscriminant::Alias,
        KindWire::Trait(_) => KindDiscriminant::Trait,
        KindWire::Impl(_) => KindDiscriminant::Impl,
        KindWire::Enum(_) => KindDiscriminant::Enum,
        KindWire::Variant(_) => KindDiscriminant::Variant,
        KindWire::Const(_) => KindDiscriminant::Const,
        KindWire::Static(_) => KindDiscriminant::Static,
        KindWire::Reexport(_) => KindDiscriminant::Reexport,
        KindWire::Param(_) => KindDiscriminant::Param,
    };
    OwnedEntryPayload::sealed(sym, disc, kind, EntryPayloadFlags::default())
}

fn round_trip(payload: &OwnedEntryPayload, parent: Option<IntroId>, links: &[LinkWire]) {
    let bytes = serialize_f1(payload, parent, links);
    let view = F1View::from_bytes(&bytes)
        .unwrap_or_else(|e| panic!("parse failed for {:?}: {:?}", payload.kind_disc, e));
    assert_eq!(view.parent(), parent, "parent mismatch");
    assert_eq!(view.links().len(), links.len(), "link count mismatch");
    let recon = view
        .to_owned_payload()
        .unwrap_or_else(|e| panic!("to_owned_payload failed: {e:?}"));
    assert_eq!(recon.kind_disc, payload.kind_disc, "kind_disc mismatch");
    assert_eq!(recon.symbol.name, payload.symbol.name, "name mismatch");
    assert_eq!(
        recon.symbol.visibility, payload.symbol.visibility,
        "vis mismatch"
    );
}

// --- C0 control bytes are never emitted except \n ---

/// §6.4 paranoia: the serialized F1 output must not contain any C0 control
/// byte except the two structural ones — TAB (`\t`, field separator) and LF
/// (`\n`, frame terminator). Everything else (all payload text) is escaped,
/// so no ESC/NUL/etc. can appear raw, which keeps libpijul on the text path.
fn assert_no_bad_controls(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        assert!(!(b < 0x20 && b != b'\n' && b != b'\t'), "control byte 0x{b:02x} at position {i} in F1 output");
    }
}

#[test]
fn f1_module_round_trip() {
    let payload = make_payload(base_sym("my_mod"), KindWire::Module(ModuleWire {}));
    round_trip(&payload, None, &[]);
    let bytes = serialize_f1(&payload, None, &[]);
    assert_no_bad_controls(&bytes);
}

#[test]
fn f1_function_round_trip() {
    let sym = SymbolWire {
        name: "my_fn".into(),
        documentation: Some("Para one.\n\nPara two.".into()),
        aliases: vec!["alias_a".into(), "alias_z".into()],
        ..base_sym("my_fn")
    };
    let kind = KindWire::Function(FunctionWire {
        input_params: Box::new([
            ParamWire {
                name: Some("x".into()),
                ty: TypeRefWire::Same(intro(1)),
            },
            ParamWire {
                name: None,
                ty: TypeRefWire::Same(intro(2)),
            },
        ]),
        output_params: Box::new([ParamWire {
            name: None,
            ty: TypeRefWire::Same(intro(3)),
        }]),
        sig: FnSigFlags {
            self_kind: SelfKind::Ref,
            is_async: true,
            is_const: false,
            is_unsafe: false,
            abi: Some("C".into()),
            variadic: false,
            defaulted: true,
        },
        generics: Box::new([GenericParamWire::Type {
            name: "T".into(),
            bounds: Box::new([TypeRefWire::Same(intro(9))]),
            default: None,
        }]),
        wheres: Box::new([WherePredWire {
            target: TypeWire::SelfType,
            bounds: Box::new([TypeRefWire::Same(intro(10))]),
        }]),
    });
    let payload = make_payload(sym, kind);
    round_trip(&payload, Some(intro(5)), &[]);
    let bytes = serialize_f1(&payload, Some(intro(5)), &[]);
    assert_no_bad_controls(&bytes);
}

#[test]
fn f1_record_round_trip() {
    let kind = KindWire::Record(RecordWire {
        form: RecordForm::Struct,
        fields: Box::new([intro(1), intro(2), intro(3)]),
        generics: Box::new([]),
        wheres: Box::new([]),
        auto: Box::new([AutoFact {
            trait_: AutoTrait::Send,
            state: AutoState::Yes,
        }]),
    });
    let payload = make_payload(base_sym("MyStruct"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_field_round_trip() {
    let kind = KindWire::Field(FieldWire {
        ty: Some(TypeRefWire::Same(intro(7))),
    });
    let payload = make_payload(base_sym("my_field"), kind);
    round_trip(&payload, Some(intro(2)), &[]);
}

#[test]
fn f1_trait_round_trip() {
    let kind = KindWire::Trait(TraitWire {
        supers: Box::new([TypeRefWire::Same(intro(1)), TypeRefWire::Same(intro(2))]),
        flags: TraitFlags {
            is_auto: false,
            is_unsafe: true,
            dyn_compat: TriState::Yes,
            sealed: Sealed::Full,
        },
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let payload = make_payload(base_sym("MyTrait"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_impl_round_trip() {
    let kind = KindWire::Impl(ImplWire {
        of: Some(TypeRefWire::Same(intro(1))),
        self_ty: TypeWire::SelfType,
        flags: ImplFlags {
            negative: false,
            blanket: true,
        },
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let payload = make_payload(base_sym("impl"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_enum_round_trip() {
    let kind = KindWire::Enum(EnumWire {
        variants: Box::new([intro(10), intro(11), intro(12)]),
        generics: Box::new([]),
        wheres: Box::new([]),
        auto: Box::new([]),
    });
    let payload = make_payload(base_sym("MyEnum"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_variant_round_trip() {
    let kind = KindWire::Variant(VariantWire {
        form: VariantForm::Tuple,
        discr: Some("42".into()),
        fields: Box::new([intro(20), intro(21)]),
    });
    let payload = make_payload(base_sym("Foo"), kind);
    round_trip(&payload, Some(intro(5)), &[]);
}

#[test]
fn f1_const_round_trip() {
    let kind = KindWire::Const(ConstWire {
        ty: TypeRefWire::Same(intro(1)),
        value: Some("42".into()),
    });
    let payload = make_payload(base_sym("MY_CONST"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_static_round_trip() {
    let kind = KindWire::Static(StaticWire {
        ty: TypeRefWire::Same(intro(2)),
        mutable: true,
    });
    let payload = make_payload(base_sym("MY_STATIC"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_reexport_round_trip() {
    let kind = KindWire::Reexport(ReexportWire {
        target: sref("cargo", "other", 3),
    });
    let payload = make_payload(base_sym("MyReexport"), kind);
    round_trip(&payload, None, &[]);
    let bytes = serialize_f1(&payload, None, &[]);
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("retgt\t"), "must have retgt line");
}

#[test]
fn f1_type_alias_round_trip() {
    let kind = KindWire::Type(TypeAliasWire {
        ty: TypeWire::Primitive(crate::wire::PrimitiveWire::Bool),
        generics: Box::new([]),
        wheres: Box::new([]),
        auto: Box::new([]),
    });
    let payload = make_payload(base_sym("MyAlias"), kind);
    round_trip(&payload, None, &[]);
}

#[test]
fn f1_escape_round_trip() {
    // All C0 controls, non-ASCII, tabs, newlines in strings
    let mut sym = base_sym("weird\x00\x01\x1Fname");
    sym.documentation = Some("doc\twith\ttabs\n\nand\nnewlines".into());
    sym.source_path = "path with \r carriage return".into();
    sym.aliases = vec!["a\tb".into()];
    let payload = make_payload(sym, KindWire::Module(ModuleWire {}));
    let bytes = serialize_f1(&payload, None, &[]);
    assert_no_bad_controls(&bytes);
    let view = F1View::from_bytes(&bytes).expect("parse");
    let recon = view.to_owned_payload().expect("to_owned");
    assert_eq!(recon.symbol.name, "weird\x00\x01\x1Fname");
    assert_eq!(recon.symbol.source_path, "path with \r carriage return");
}

#[test]
fn f1_non_ascii_names() {
    let sym = base_sym("日本語クラス");
    let payload = make_payload(sym, KindWire::Module(ModuleWire {}));
    round_trip(&payload, None, &[]);
    let bytes = serialize_f1(&payload, None, &[]);
    assert_no_bad_controls(&bytes);
}

#[test]
fn f1_large_doc_one_para() {
    // A single doc paragraph that is very large — should be one doc line.
    let big_doc = "x".repeat(50_000);
    let mut sym = base_sym("big");
    sym.documentation = Some(big_doc.clone());
    let payload = make_payload(sym, KindWire::Module(ModuleWire {}));
    let bytes = serialize_f1(&payload, None, &[]);
    assert_no_bad_controls(&bytes);
    let view = F1View::from_bytes(&bytes).expect("parse");
    let recon = view.to_owned_payload().expect("to_owned");
    assert_eq!(recon.symbol.documentation, Some(big_doc));
}

#[test]
fn f1_10k_params() {
    // 10k input params — verify determinism and no control bytes.
    let params: Vec<ParamWire> = (0u32..10_000)
        .map(|i| ParamWire {
            name: Some(format!("p{i}")),
            ty: TypeRefWire::Same(IntroId::from_raw({
                let mut b = [0u8; 32];
                b[0] = (i & 0xFF) as u8;
                b[1] = ((i >> 8) & 0xFF) as u8;
                b
            })),
        })
        .collect();
    let kind = KindWire::Function(FunctionWire {
        input_params: params.into_boxed_slice(),
        output_params: Box::new([]),
        sig: FnSigFlags::default(),
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let payload = make_payload(base_sym("big_fn"), kind);
    let b1 = serialize_f1(&payload, None, &[]);
    let b2 = serialize_f1(&payload, None, &[]);
    assert_eq!(b1, b2, "must be deterministic");
    assert_no_bad_controls(&b1);
}

#[test]
fn f1_determinism() {
    // Same payload → same bytes, every time.
    let kind = KindWire::Record(RecordWire {
        form: RecordForm::Struct,
        fields: Box::new([intro(5), intro(3), intro(1)]),
        generics: Box::new([]),
        wheres: Box::new([]),
        auto: Box::new([
            AutoFact {
                trait_: AutoTrait::Send,
                state: AutoState::Yes,
            },
            AutoFact {
                trait_: AutoTrait::Sync,
                state: AutoState::No,
            },
        ]),
    });
    let mut sym = base_sym("Stable");
    sym.aliases = vec!["z".into(), "a".into(), "m".into()];
    let payload = make_payload(sym, kind);
    let b1 = serialize_f1(&payload, Some(intro(99)), &[]);
    let b2 = serialize_f1(&payload, Some(intro(99)), &[]);
    assert_eq!(b1, b2);
}

#[test]
fn f1_all_12_discriminants() {
    // Quick smoke test for all discriminants
    let kinds: Vec<KindWire> = vec![
        KindWire::Module(ModuleWire {}),
        KindWire::Record(RecordWire {
            form: RecordForm::Struct,
            fields: Box::new([]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        KindWire::Field(FieldWire { ty: None }),
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        KindWire::Type(TypeAliasWire {
            ty: TypeWire::Any,
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        KindWire::Trait(TraitWire {
            supers: Box::new([]),
            flags: TraitFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        KindWire::Impl(ImplWire {
            of: None,
            self_ty: TypeWire::SelfType,
            flags: ImplFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        KindWire::Enum(EnumWire {
            variants: Box::new([]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        KindWire::Variant(VariantWire {
            form: VariantForm::Unit,
            discr: None,
            fields: Box::new([]),
        }),
        KindWire::Const(ConstWire {
            ty: TypeRefWire::Same(intro(0)),
            value: None,
        }),
        KindWire::Static(StaticWire {
            ty: TypeRefWire::Same(intro(0)),
            mutable: false,
        }),
        KindWire::Reexport(ReexportWire {
            target: sref("cargo", "x", 0),
        }),
    ];
    for kind in &kinds {
        let disc = match kind {
            KindWire::Module(_) => KindDiscriminant::Module,
            KindWire::Record(_) => KindDiscriminant::Record,
            KindWire::Field(_) => KindDiscriminant::Field,
            KindWire::Function(_) => KindDiscriminant::Function,
            KindWire::Type(_) => KindDiscriminant::Alias,
            KindWire::Trait(_) => KindDiscriminant::Trait,
            KindWire::Impl(_) => KindDiscriminant::Impl,
            KindWire::Enum(_) => KindDiscriminant::Enum,
            KindWire::Variant(_) => KindDiscriminant::Variant,
            KindWire::Const(_) => KindDiscriminant::Const,
            KindWire::Static(_) => KindDiscriminant::Static,
            KindWire::Reexport(_) => KindDiscriminant::Reexport,
            KindWire::Param(_) => KindDiscriminant::Param,
        };
        let payload = OwnedEntryPayload::sealed(
            base_sym("x"),
            disc,
            kind.clone(),
            EntryPayloadFlags::default(),
        );
        let bytes = serialize_f1(&payload, None, &[]);
        assert_no_bad_controls(&bytes);
        let view = F1View::from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("parse failed for {disc:?}: {e:?}"));
        assert_eq!(view.kind_disc(), disc);
        let _ = view.to_owned_payload().expect("to_owned_payload");
    }
}

#[test]
fn f1_links_round_trip() {
    let payload = make_payload(base_sym("f"), KindWire::Module(ModuleWire {}));
    let links = vec![
        LinkWire {
            other: sref("cargo", "pkg_a", 1),
            kind_self: KindDiscriminant::Function,
            kind_other: KindDiscriminant::Trait,
        },
        LinkWire {
            other: sref("npm", "pkg_b", 2),
            kind_self: KindDiscriminant::Field,
            kind_other: KindDiscriminant::Record,
        },
    ];
    let bytes = serialize_f1(&payload, None, &links);
    let view = F1View::from_bytes(&bytes).expect("parse");
    assert_eq!(view.links().len(), 2);
}

#[test]
fn f1_strict_parser_rejects_unknown_key() {
    let bytes = b"NdIrF1\t1\nname\tfoo\nvis\tpublic\nkind\tmodule\nspan\t0\t1\nunknown\tvalue\n";
    assert!(matches!(
        F1View::from_bytes(bytes),
        Err(Error::UnknownKey(_))
    ));
}

#[test]
fn f1_strict_parser_rejects_wrong_order() {
    // vis before name violates registry order
    let bytes = b"NdIrF1\t1\nvis\tpublic\nname\tfoo\nkind\tmodule\nspan\t0\t1\n";
    assert!(matches!(
        F1View::from_bytes(bytes),
        Err(Error::OutOfOrder(_))
    ));
}

#[test]
fn f1_strict_parser_rejects_unsorted_set() {
    // Two alias lines in wrong order
    let bytes =
        b"NdIrF1\t1\nname\tfoo\nvis\tpublic\nkind\tmodule\nspan\t0\t1\nalias\tzz\nalias\taa\n";
    assert!(matches!(
        F1View::from_bytes(bytes),
        Err(Error::UnsortedSet(_, _, _))
    ));
}

#[test]
fn api_surface_hash_excludes_name_parent() {
    // Same shape but different names → same api_surface_hash
    let kind = KindWire::Function(FunctionWire {
        input_params: Box::new([ParamWire {
            name: None,
            ty: TypeRefWire::Same(intro(1)),
        }]),
        output_params: Box::new([]),
        sig: FnSigFlags::default(),
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let p1 = make_payload(base_sym("fn_one"), kind.clone());
    let p2 = make_payload(base_sym("fn_two"), kind);
    let h1 = compute_api_surface_hash(&p1);
    let h2 = compute_api_surface_hash(&p2);
    assert_eq!(h1, h2, "api_surface_hash must be name-independent");
}
