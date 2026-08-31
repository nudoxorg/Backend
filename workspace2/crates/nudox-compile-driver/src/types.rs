use core::num::TryFromIntError;

use blake3::Hasher;
use nudox_compile_vocab::Language;
use nudox_ir_format::{
    EntityRecord, FragmentError, FragmentView, PrepareError, PreparedFragment, PrimitiveType,
    TypeNode, WriteError,
};
use nudox_ir_vocab::TypeId;
use thiserror::Error;

use crate::native::parse_with_native_tool;

/// One immutable source fact carried through parser admission and IR publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    /// BLAKE3 digest of the exact source bytes passed to the native tool.
    pub digest: [u8; blake3::OUT_LEN],
    /// Exact source length established before any native work begins.
    pub byte_len: u32,
}

/// The concrete native tool selected by the closed language dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTool {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
    CSharpCompiler,
}

/// Immutable compile facts supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileRequest<'source> {
    /// Closed source language family selected at the static dispatch boundary.
    pub language: Language,
    /// Exact UTF-8-or-binary source bytes whose identity is retained in every terminal.
    pub source: &'source [u8],
}

/// One caller-owned canonical IR output region reused across compile calls.
pub struct CompileScratch<'output> {
    /// Output storage for the compact validated-source fragment.
    pub fragment_output: &'output mut [u8],
}

/// Borrowed compact IR emitted only after native parser admission succeeds.
pub struct CompiledFragment<'artifact> {
    /// Closed language family that dispatched exactly one concrete adapter.
    pub language: Language,
    /// Exact source identity retained by both success and native failure terminals.
    pub source: SourceIdentity,
    /// Validated compact IR borrowing the caller-owned output region.
    pub fragment: FragmentView<'artifact>,
}

/// Exact compile terminal with source-bearing native causes.
#[derive(Debug, Error)]
pub enum CompileFailure {
    /// The source length could not fit the compact semantic identity width.
    #[error("source has {actual} bytes, which exceeds the compact source identity width")]
    SourceLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    /// The selected language has no locally reproducible native tooling adapter.
    #[error(
        "{language:?} has no locally reproducible {tool:?} adapter for source {source_identity:?}"
    )]
    ToolingUnavailable {
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
    },
    /// Starting the native parser process preserved its concrete I/O cause.
    #[error("could not start {tool:?} for {language:?} source {source_identity:?}")]
    ToolStart {
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
        #[source]
        cause: std::io::Error,
    },
    /// Writing the exact borrowed source to native stdin preserved its concrete I/O cause.
    #[error("could not send {language:?} source {source_identity:?} to {tool:?}")]
    ToolInput {
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
        #[source]
        cause: std::io::Error,
    },
    /// Waiting for the native parser process preserved its concrete I/O cause.
    #[error("could not observe {tool:?} for {language:?} source {source_identity:?}")]
    ToolWait {
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
        #[source]
        cause: std::io::Error,
    },
    /// The native parser rejected this exact source with its concrete process status.
    #[error("{tool:?} rejected {language:?} source {source_identity:?} with {status:?}")]
    NativeRejected {
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
        status: std::process::ExitStatus,
    },
    /// Lowering could not prepare the compact IR from its typed facts.
    #[error("could not prepare compact IR")]
    Prepare(#[from] PrepareError),
    /// The caller-owned output region could not hold the prepared compact IR.
    #[error("could not write compact IR")]
    Write(#[from] WriteError),
    /// The freshly written compact IR failed its own borrowed validation.
    #[error("fresh compact IR failed validation")]
    Validate(#[from] FragmentError),
}

/// Parses exact source with one closed native adapter, then lowers its validated-source fact.
pub fn compile<'source, 'output>(
    request: CompileRequest<'source>,
    scratch: CompileScratch<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure> {
    let source = SourceIdentity::from_bytes(request.source)?;
    parse_with_native_tool(request.language, source, request.source)?;
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::I32)];
    let prepared = PreparedFragment::prepare(&entities, &nodes)?;
    let bytes = prepared.write_into(scratch.fragment_output)?;
    let fragment = FragmentView::validate(bytes)?;
    Ok(CompiledFragment {
        language: request.language,
        source,
        fragment,
    })
}

impl SourceIdentity {
    fn from_bytes(source_bytes: &[u8]) -> Result<Self, CompileFailure> {
        let byte_len =
            u32::try_from(source_bytes.len()).map_err(|source| CompileFailure::SourceLength {
                actual: source_bytes.len(),
                source,
            })?;
        let mut hasher = Hasher::new();
        hasher.update(source_bytes);
        Ok(Self {
            digest: *hasher.finalize().as_bytes(),
            byte_len,
        })
    }
}
