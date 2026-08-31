use std::{
    io::Write,
    process::{Command, Stdio},
};

use nudox_compile_vocab::Language;

use crate::types::{CompileFailure, NativeTool, SourceIdentity};

trait NativeFrontend {
    const LANGUAGE: Language;
    const TOOL: NativeTool;

    fn command() -> Command;
}

trait NativeTerminal {
    const LANGUAGE: Language;
    const TOOL: NativeTool;
}

struct RustFrontend;
struct ClangFrontend;
struct PythonFrontend;
struct TypeScriptFrontend;
struct GoFrontend;
struct JavaFrontend;
struct CSharpFrontend;

impl NativeFrontend for RustFrontend {
    const LANGUAGE: Language = Language::Rust;
    const TOOL: NativeTool = NativeTool::Rustc;

    fn command() -> Command {
        let mut command = Command::new("rustc");
        command.args([
            "--crate-type=lib",
            "--edition=2024",
            "--emit=metadata=-",
            "-",
        ]);
        command
    }
}

impl NativeFrontend for ClangFrontend {
    const LANGUAGE: Language = Language::Clang;
    const TOOL: NativeTool = NativeTool::Clang;

    fn command() -> Command {
        let mut command = Command::new("clang");
        command.args(["-x", "c", "-fsyntax-only", "-"]);
        command
    }
}

impl NativeFrontend for PythonFrontend {
    const LANGUAGE: Language = Language::Python;
    const TOOL: NativeTool = NativeTool::Python;

    fn command() -> Command {
        let mut command = Command::new("python3");
        command.args([
            "-c",
            "import sys; compile(sys.stdin.read(), '<nudox>', 'exec')",
        ]);
        command
    }
}

impl NativeTerminal for TypeScriptFrontend {
    const LANGUAGE: Language = Language::TypeScript;
    const TOOL: NativeTool = NativeTool::TypeScriptCompiler;
}

impl NativeTerminal for GoFrontend {
    const LANGUAGE: Language = Language::Go;
    const TOOL: NativeTool = NativeTool::GoCompiler;
}

impl NativeTerminal for JavaFrontend {
    const LANGUAGE: Language = Language::Java;
    const TOOL: NativeTool = NativeTool::JavaCompiler;
}

impl NativeTerminal for CSharpFrontend {
    const LANGUAGE: Language = Language::CSharp;
    const TOOL: NativeTool = NativeTool::CSharpCompiler;
}

pub(crate) fn parse_with_native_tool(
    language: Language,
    source: SourceIdentity,
    source_bytes: &[u8],
) -> Result<(), CompileFailure> {
    match language {
        Language::Rust => drive::<RustFrontend>(source, source_bytes),
        Language::TypeScript => unavailable::<TypeScriptFrontend>(source),
        Language::Python => drive::<PythonFrontend>(source, source_bytes),
        Language::Go => unavailable::<GoFrontend>(source),
        Language::Java => unavailable::<JavaFrontend>(source),
        Language::CSharp => unavailable::<CSharpFrontend>(source),
        Language::Clang => drive::<ClangFrontend>(source, source_bytes),
    }
}

fn unavailable<ConcreteTerminal: NativeTerminal>(
    source: SourceIdentity,
) -> Result<(), CompileFailure> {
    Err(CompileFailure::ToolingUnavailable {
        language: ConcreteTerminal::LANGUAGE,
        tool: ConcreteTerminal::TOOL,
        source_identity: source,
    })
}

fn drive<ConcreteFrontend: NativeFrontend>(
    source: SourceIdentity,
    source_bytes: &[u8],
) -> Result<(), CompileFailure> {
    let mut command = ConcreteFrontend::command();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|cause| CompileFailure::ToolStart {
        language: ConcreteFrontend::LANGUAGE,
        tool: ConcreteFrontend::TOOL,
        source_identity: source,
        cause,
    })?;
    let Some(mut input) = child.stdin.take() else {
        return Err(CompileFailure::ToolInput {
            language: ConcreteFrontend::LANGUAGE,
            tool: ConcreteFrontend::TOOL,
            source_identity: source,
            cause: std::io::Error::other("native parser did not expose stdin"),
        });
    };
    input
        .write_all(source_bytes)
        .map_err(|cause| CompileFailure::ToolInput {
            language: ConcreteFrontend::LANGUAGE,
            tool: ConcreteFrontend::TOOL,
            source_identity: source,
            cause,
        })?;
    drop(input);
    let status = child.wait().map_err(|cause| CompileFailure::ToolWait {
        language: ConcreteFrontend::LANGUAGE,
        tool: ConcreteFrontend::TOOL,
        source_identity: source,
        cause,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CompileFailure::NativeRejected {
            language: ConcreteFrontend::LANGUAGE,
            tool: ConcreteFrontend::TOOL,
            source_identity: source,
            status,
        })
    }
}
