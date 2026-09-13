//! Ordinary public-contract tests for canonical semantic type output.

use core::num::NonZeroUsize;

use compiler_ir::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, ConcreteType, Ir, IrBuilder, LiteralType,
    TypeId, prepare_canonical_type,
};

#[derive(Debug)]
enum TestError {
    Build(compiler_ir::BuildError),
    Render(CanonicalTypeRenderError),
    MissingLimit,
    UnexpectedSuccess,
}

impl From<compiler_ir::BuildError> for TestError {
    fn from(source: compiler_ir::BuildError) -> Self {
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
    let leaf = builder.intern_concrete(ConcreteType::Builtin(compiler_ir::BuiltinType::Bool))?;
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
