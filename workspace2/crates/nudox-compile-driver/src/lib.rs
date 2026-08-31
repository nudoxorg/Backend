//! Native parse admission followed by compact validated-source IR lowering.

mod native;
mod types;

pub use types::{
    CompileFailure, CompileRequest, CompileScratch, CompiledFragment, NativeTool, SourceIdentity,
    compile,
};
