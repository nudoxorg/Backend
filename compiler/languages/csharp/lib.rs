//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It separates unavailable tooling from malformed helper output.
//! Recorded fixtures prove the offline semantic terminal.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod oracle;

pub use oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
