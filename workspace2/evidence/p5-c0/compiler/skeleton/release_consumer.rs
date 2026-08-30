use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{FrontendError, Language, Stage};

enum ConsumerError {
    Frontend(FrontendError),
    PointerMismatch,
    LengthMismatch,
}

impl core::fmt::Debug for ConsumerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Frontend(error) => formatter.debug_tuple("Frontend").field(error).finish(),
            Self::PointerMismatch => formatter.write_str("pointer mismatch"),
            Self::LengthMismatch => formatter.write_str("length mismatch"),
        }
    }
}

fn verify<'source>(
    result: Result<&'source [u8], FrontendError>,
    source: &'source [u8],
) -> Result<&'source [u8], ConsumerError> {
    let output = match result {
        Ok(output) => output,
        Err(error) => return Err(ConsumerError::Frontend(error)),
    };
    if !core::ptr::eq(output, source) {
        return Err(ConsumerError::PointerMismatch);
    }
    if output.len() != source.len() {
        return Err(ConsumerError::LengthMismatch);
    }
    Ok(output)
}

#[inline(never)]
fn rust_parse(source: &[u8]) -> Result<&[u8], ConsumerError> {
    verify(
        FullRegistry.dispatch(Language::RustSubset, Stage::Parse, source),
        source,
    )
}

#[inline(never)]
fn rust_lower(source: &[u8]) -> Result<&[u8], ConsumerError> {
    verify(
        FullRegistry.dispatch(Language::RustSubset, Stage::LowerIr, source),
        source,
    )
}

#[inline(never)]
fn typescript_parse(source: &[u8]) -> Result<&[u8], ConsumerError> {
    verify(
        FullRegistry.dispatch(Language::TypeScriptSubset, Stage::Parse, source),
        source,
    )
}

fn main() -> Result<(), ConsumerError> {
    let rust_parse_input = core::hint::black_box(b"fn release_parse() {}".as_slice());
    let rust_lower_input = core::hint::black_box(b"fn release_lower() {}".as_slice());
    let typescript_parse_input = core::hint::black_box(b"const release_parse = 1;".as_slice());

    core::hint::black_box(rust_parse(rust_parse_input)?);
    core::hint::black_box(rust_lower(rust_lower_input)?);
    core::hint::black_box(typescript_parse(typescript_parse_input)?);
    Ok(())
}
