//! Ordinary public-contract tests for canonical semantic type output.

use core::num::NonZeroUsize;

use backend_semantic::ir::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, ConcreteType, Ir, IrBuilder, LiteralType,
    TypeId, prepare_canonical_type,
};

#[derive(Debug)]
enum TestError {
    Build(backend_semantic::ir::BuildError),
    Render(CanonicalTypeRenderError),
    MissingLimit,
    UnexpectedSuccess,
}

impl From<backend_semantic::ir::BuildError> for TestError {
    fn from(source: backend_semantic::ir::BuildError) -> Self {
        Self::Build(source)
    }
}

impl From<CanonicalTypeRenderError> for TestError {
    fn from(source: CanonicalTypeRenderError) -> Self {
        Self::Render(source)
    }
}

impl core::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Build(source) => source.fmt(formatter),
            Self::Render(source) => source.fmt(formatter),
            Self::MissingLimit => formatter.write_str("test traversal limit is zero"),
            Self::UnexpectedSuccess => formatter.write_str("render unexpectedly succeeded"),
        }
    }
}

impl core::error::Error for TestError {}

fn limits(depth: usize) -> Result<CanonicalTypeRenderLimits, TestError> {
    NonZeroUsize::new(depth)
        .map(CanonicalTypeRenderLimits::new)
        .ok_or(TestError::MissingLimit)
}

fn literal_image(unrelated_first: bool) -> Result<(Ir, TypeId), TestError> {
    let mut builder = IrBuilder::new();
    if unrelated_first {
        let _unrelated = builder.intern_atom(b"unrelated")?;
    }
    let spelling = builder.intern_atom(b"order-independent")?;
    if !unrelated_first {
        let _unrelated = builder.intern_atom(b"unrelated")?;
    }
    let root = builder.intern_concrete(ConcreteType::Literal(LiteralType::String(spelling)))?;
    Ok((builder.finish()?, root.erase()))
}

fn render<'output>(
    image: &Ir,
    root: TypeId,
    output: &'output mut [u8],
) -> Result<&'output str, TestError> {
    let prepared = prepare_canonical_type(image, root, limits(16)?)?;
    Ok(prepared.write_into(output)?)
}

#[test]
fn canonical_type_output_ignores_raw_atom_insertion_order() -> Result<(), TestError> {
    let (left, left_root) = literal_image(true)?;
    let (right, right_root) = literal_image(false)?;
    let mut left_output = [0_u8; 256];
    let mut right_output = [0_u8; 256];

    let left_text = render(&left, left_root, &mut left_output)?;
    let right_text = render(&right, right_root, &mut right_output)?;

    assert_eq!(left_text, right_text);
    assert_eq!(
        left_text,
        "literal.string(x\"6F726465722D696E646570656E64656E74\")"
    );
    Ok(())
}

#[test]
fn preparation_rejects_small_output_without_touching_it() -> Result<(), TestError> {
    let (image, root) = literal_image(true)?;
    let prepared = prepare_canonical_type(&image, root, limits(16)?)?;
    let mut output = [0xA5_u8; 8];
    let before = output;

    match prepared.write_into(&mut output) {
        Err(CanonicalTypeRenderError::OutputTooSmall {
            root: observed,
            required,
            available: 8,
        }) if observed == root && required == prepared.encoded_len => {}
        Err(source) => return Err(TestError::Render(source)),
        Ok(_) => return Err(TestError::UnexpectedSuccess),
    }
    assert_eq!(output, before);
    Ok(())
}

#[test]
fn recursive_rendering_names_the_exact_depth_boundary() -> Result<(), TestError> {
    let mut builder = IrBuilder::new();
    let leaf = builder.intern_concrete(ConcreteType::Builtin(
        backend_semantic::ir::BuiltinType::Bool,
    ))?;
    let first = builder.intern_concrete(ConcreteType::Slice(leaf.erase()))?;
    let second = builder.intern_concrete(ConcreteType::Slice(first.erase()))?;
    let root = builder.intern_concrete(ConcreteType::Slice(second.erase()))?;
    let image = builder.finish()?;

    match prepare_canonical_type(&image, root.erase(), limits(2)?) {
        Err(CanonicalTypeRenderError::TraversalLimit {
            root: observed_root,
            at,
            limit,
        }) if observed_root == root.erase() && at == first.erase() && limit.get() == 2 => {}
        Err(source) => return Err(TestError::Render(source)),
        Ok(_) => return Err(TestError::UnexpectedSuccess),
    }
    Ok(())
}

#[test]
fn production_depth_limit_renders_boundary_and_refuses_deeper_types_without_recursion()
-> Result<(), TestError> {
    let mut builder = IrBuilder::new();
    let leaf = builder
        .intern_concrete(ConcreteType::Builtin(
            backend_semantic::ir::BuiltinType::Bool,
        ))?
        .erase();
    let mut root = leaf;
    for _ in 1..256 {
        root = builder
            .intern_concrete(ConcreteType::Optional(root))?
            .erase();
    }
    let too_deep = builder
        .intern_concrete(ConcreteType::Optional(root))?
        .erase();
    let image = builder.finish()?;
    let prepared = prepare_canonical_type(&image, root, limits(256)?)?;
    let expected = format!(
        "{}builtin(bool){}",
        "optional(".repeat(255),
        ")".repeat(255)
    );
    assert_eq!(prepared.encoded_len, expected.len());
    let mut bytes = vec![0; prepared.encoded_len];
    assert_eq!(prepared.write_into(&mut bytes)?, expected);
    let mut streamed = String::new();
    prepared.write_to(&mut streamed)?;
    assert_eq!(streamed, expected);
    assert!(matches!(
        prepare_canonical_type(&image, too_deep, limits(256)?),
        Err(CanonicalTypeRenderError::TraversalLimit { root: observed, at, limit })
            if observed == too_deep && at == leaf && limit.get() == 256
    ));
    Ok(())
}

#[test]
fn cyclic_type_graphs_terminate_at_the_selected_depth_and_missing_roots_remain_typed()
-> Result<(), TestError> {
    let mut builder = IrBuilder::new();
    let root = builder
        .intern_concrete(ConcreteType::Optional(TypeId::new(0)))?
        .erase();
    assert_eq!(root, TypeId::new(0));
    let image = builder.finish()?;
    assert!(matches!(
        prepare_canonical_type(&image, root, limits(256)?),
        Err(CanonicalTypeRenderError::TraversalLimit { root: observed, at, limit })
            if observed == root && at == root && limit.get() == 256
    ));
    let absent = TypeId::new(1);
    assert!(matches!(
        prepare_canonical_type(&image, absent, limits(256)?),
        Err(CanonicalTypeRenderError::MissingRoot { root }) if root == absent
    ));
    Ok(())
}

fn measured_flat_union(
    width: usize,
) -> Result<(usize, allocation_counter::AllocationInfo), TestError> {
    let mut builder = IrBuilder::new();
    let leaf = builder
        .intern_concrete(ConcreteType::Builtin(
            backend_semantic::ir::BuiltinType::Bool,
        ))?
        .erase();
    let members = builder.intern_types(&vec![leaf; width])?;
    let root = builder
        .intern_concrete(ConcreteType::Union(members))?
        .erase();
    let image = builder.finish()?;
    let selected_limits = limits(2)?;
    let mut measured = Err(TestError::UnexpectedSuccess);
    let allocation = allocation_counter::measure(|| {
        measured = prepare_canonical_type(&image, root, selected_limits)
            .map(|prepared| prepared.encoded_len)
            .map_err(TestError::from);
    });
    Ok((measured?, allocation))
}

#[test]
fn wide_type_lists_keep_pending_memory_independent_of_sibling_count() -> Result<(), TestError> {
    let (short_len, short) = measured_flat_union(1)?;
    let (wide_len, wide) = measured_flat_union(65_536)?;
    assert_eq!(short_len, "union[builtin(bool)]".len());
    assert_eq!(wide_len, 6 + 65_536 * 14);
    // Input construction is outside the measurement. A renderer that queues
    // all siblings needs width-proportional memory even though depth is two.
    assert_eq!(wide.bytes_max, short.bytes_max, "{short:?} / {wide:?}");
    assert_eq!(wide.bytes_total, short.bytes_total, "{short:?} / {wide:?}");
    Ok(())
}

#[test]
fn pooled_type_continuations_preserve_golden_order_and_formatter_refusal() -> Result<(), TestError>
{
    use backend_semantic::ir::{
        BuiltinType, ComputedType, MappedModifier, ObjectMember, PropertyKey, TemplatePart,
        TupleElement, TupleElementKind, VariadicForm,
    };
    let mut builder = IrBuilder::new();
    let name = builder.intern_atom(b"a")?;
    let bool_ty = builder
        .intern_concrete(ConcreteType::Builtin(BuiltinType::Bool))?
        .erase();
    let types = builder.intern_types(&[bool_ty, bool_ty])?;
    let union = builder.intern_concrete(ConcreteType::Union(types))?.erase();
    let parts = builder
        .intern_template_parts(&[TemplatePart::Bytes(name), TemplatePart::Placeholder(union)])?;
    let template = builder
        .intern_computed(ComputedType::TemplateLiteral(parts))?
        .erase();
    let parameters = builder.intern_tuple_elements(&[
        TupleElement {
            label: Some(name),
            ty: template,
            kind: TupleElementKind::Required,
        },
        TupleElement {
            label: None,
            ty: bool_ty,
            kind: TupleElementKind::Rest,
        },
    ])?;
    let results = builder.intern_tuple_elements(&[])?;
    let function = builder
        .intern_concrete(ConcreteType::Function {
            parameters,
            results,
            abi: Some(name),
            variadic: VariadicForm::TypedLast,
            unsafe_: true,
        })?
        .erase();
    let members = builder.intern_object_members(&[
        ObjectMember::Property {
            key: PropertyKey::Computed(bool_ty),
            ty: union,
            optional: true,
            readonly: false,
        },
        ObjectMember::Method {
            key: PropertyKey::Private(name),
            signature: function,
            optional: false,
        },
        ObjectMember::Index {
            parameter: name,
            key: bool_ty,
            value: template,
            readonly: true,
        },
        ObjectMember::Call(function),
        ObjectMember::Construct(function),
    ])?;
    let object = builder
        .intern_concrete(ConcreteType::Object(members))?
        .erase();
    let mapped = builder
        .intern_computed(ComputedType::Mapped {
            parameter: name,
            constraint: object,
            name_as: None,
            value: template,
            readonly: MappedModifier::Add,
            optional: MappedModifier::Remove,
        })?
        .erase();
    let image = builder.finish()?;
    let union_text = "union[builtin(bool),builtin(bool)]";
    let template_text = format!("template[bytes(x\"61\"),placeholder({union_text})]");
    let function_text = format!(
        "function(parameters=[element(kind=required,label=x\"61\",type={template_text}),element(kind=rest,label=none,type=builtin(bool))],results=[],abi=x\"61\",variadic=typed-last,unsafe=true)"
    );
    let expected = format!(
        "mapped(parameter=x\"61\",constraint=object[property(key=computed(builtin(bool)),type={union_text},optional=true,readonly=false),method(key=private(x\"61\"),signature={function_text},optional=false),index(parameter=x\"61\",key=builtin(bool),value={template_text},readonly=true),call({function_text}),construct({function_text})],as=none,value={template_text},readonly=add,optional=remove)"
    );
    let prepared = prepare_canonical_type(&image, mapped, limits(32)?)?;
    let mut bytes = vec![0; prepared.encoded_len];
    assert_eq!(prepared.write_into(&mut bytes)?, expected);
    let mut streamed = String::new();
    prepared.write_to(&mut streamed)?;
    assert_eq!(streamed, expected);
    struct Refuses;
    impl core::fmt::Write for Refuses {
        fn write_str(&mut self, _: &str) -> core::fmt::Result {
            Err(core::fmt::Error)
        }
    }
    assert!(matches!(
        prepared.write_to(&mut Refuses),
        Err(CanonicalTypeRenderError::OutputWrite { root }) if root == mapped
    ));
    Ok(())
}
