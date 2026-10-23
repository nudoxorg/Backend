//! Human text for the exact terminal one package source reached.
//!
//! A failed source is reported to people as `package semantic compilation failed
//! for <file>: <text>`. The text names the phase the source stopped in and the
//! actionable cause, and is built only from closed vocabularies and bounded,
//! already-sanitized details: the compiler's typed terminal is never rendered
//! with its derived `Debug`, which would spell internal structure and content
//! hashes instead of an explanation.

use std::fmt::{self, Write as _};

use backend_library::PackageCompilerFailure;
use backend_library::interface::{
    CompilerCause, CompilerDiagnostic, CompilerRuntimeCause, CompilerTerminal, FragmentCause,
    NativeArtifactAction, NativeDirectoryCause, NativeIoFact, NativeWorkCause, NativeWorkPhase,
    PackageCompilePhase, PublicationCause, PublicationPhase,
};
use backend_semantic::vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, Language, NativeTool, Stage,
};

/// Largest native diagnostic prefix repeated in a failure line.
const MAX_DIAGNOSTIC_TEXT_BYTES: usize = 1024;

/// Display adapter for one package source's compiler terminal.
///
/// `path` is the package-relative source the terminal belongs to; it selects the
/// typed failure projection that owns the actionable setup wording.
pub(crate) struct TerminalText<'a> {
    path: &'a str,
    terminal: &'a CompilerTerminal,
}

impl<'a> TerminalText<'a> {
    /// Renders `terminal` for the package source at `path`.
    pub(crate) const fn new(path: &'a str, terminal: &'a CompilerTerminal) -> Self {
        Self { path, terminal }
    }
}

impl fmt::Display for TerminalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(Some(failure)) = PackageCompilerFailure::from_package_terminal(self.path, self.terminal) {
            return write_typed_failure(formatter, &failure);
        }
        write_terminal(formatter, self.terminal)
    }
}

/// A failure the library projects into its closed, sanitized vocabulary.
fn write_typed_failure(
    formatter: &mut fmt::Formatter<'_>,
    failure: &PackageCompilerFailure,
) -> fmt::Result {
    use backend_library::PackageCompilerFailurePhase as Phase;
    let phase = match failure.phase() {
        Phase::Setup => "setup",
        Phase::Authority => "language authority",
        Phase::Lowering => "lowering",
        Phase::Prepare => "compact IR preparation",
        Phase::Write => "compact IR output",
        Phase::Validate => "compact IR validation",
    };
    write!(formatter, "{phase} refused it ({}): {}", failure.kind_tag(), failure.detail())?;
    if failure.detail_truncated() {
        formatter.write_str(" [detail truncated]")?;
    }
    Ok(())
}

fn write_terminal(formatter: &mut fmt::Formatter<'_>, terminal: &CompilerTerminal) -> fmt::Result {
    match terminal {
        CompilerTerminal::PackageSource { phase, .. } => write!(
            formatter,
            "the package source could not be entered while it was being {}",
            package_phase(*phase)
        ),
        CompilerTerminal::PackageCancelled { phase, .. } => write!(
            formatter,
            "the request was cancelled while the package was being {}",
            package_phase(*phase)
        ),
        CompilerTerminal::Runtime { language, cause, .. } => write!(
            formatter,
            "the {} compiler owner could not finish the request: {}",
            language_name(*language),
            runtime_cause(cause)
        ),
        CompilerTerminal::SourceLength { actual } => write!(
            formatter,
            "the source is {actual} bytes, more than a semantic fact can address"
        ),
        CompilerTerminal::Unavailable {
            language: Language::CSharp,
            ..
        } => formatter.write_str(
            "C# semantic authority is unavailable in this owner/configuration; it requires an admitted NUDOX_DOTNET executable and a compatible published NUDOX_ROSLYN_HELPER assembly",
        ),
        CompilerTerminal::Unavailable { language, stage } => write!(
            formatter,
            "no {} compiler is available for the {} stage in this build",
            language_name(*language),
            stage_name(*stage)
        ),
        CompilerTerminal::UnsupportedStage { .. } => {
            formatter.write_str("the requested compiler stage is not implemented for this language")
        }
        CompilerTerminal::DeadlineConstruction { timeout, .. } => write!(
            formatter,
            "the compile timeout of {timeout:?} cannot be represented by this platform's clock"
        ),
        CompilerTerminal::Cancelled { diagnostic, .. } => {
            formatter.write_str("compilation was cancelled")?;
            write_diagnostic(formatter, "; tool output", diagnostic.as_ref())
        }
        CompilerTerminal::Compile { cause, .. } => write_cause(formatter, cause),
        CompilerTerminal::Publication { cause, .. } => write_publication(formatter, *cause),
        // Setup refusals own typed wording; reaching here means the projection
        // rejected the value, so name the family without guessing at detail.
        CompilerTerminal::Toolchain { selected, .. }
        | CompilerTerminal::ToolingUnavailable { tool: selected, .. } => write!(
            formatter,
            "the {} toolchain is not usable for this package",
            tool_name(*selected)
        ),
        CompilerTerminal::RequiredTool { issue, .. } => write!(
            formatter,
            "{} is required and is missing or failed its probe; set {}",
            issue.executable(),
            issue.configuration_variable()
        ),
        CompilerTerminal::AuthorityAdmission { phase, class, diagnostic, .. } => {
            write!(
                formatter,
                "the language authority could not be admitted ({}, {})",
                authority_phase(*phase),
                authority_class(*class)
            )?;
            write_diagnostic(formatter, ": ", diagnostic.as_ref())
        }
    }
}

fn write_cause(formatter: &mut fmt::Formatter<'_>, cause: &CompilerCause) -> fmt::Result {
    match cause {
        CompilerCause::Authority { phase, class, diagnostic } => {
            write!(
                formatter,
                "the language authority refused it ({}, {})",
                authority_phase(*phase),
                authority_class(*class)
            )?;
            write_diagnostic(formatter, ": ", diagnostic.as_ref())
        }
        CompilerCause::NativeWork(work) => write_native_work(formatter, work),
        CompilerCause::NativeIo { cause, .. } => write!(
            formatter,
            "the native compiler process failed: {}",
            io_fact(*cause)
        ),
        CompilerCause::NativeRejected { code, diagnostic } => {
            formatter.write_str("the native compiler rejected the source")?;
            if let Some(code) = code {
                write!(formatter, " (exit status {code})")?;
            }
            write_diagnostic(formatter, ": ", diagnostic.as_ref())
        }
        CompilerCause::DeadlineExceeded { diagnostic } => {
            formatter.write_str(
                "the compile deadline passed before this source finished; \
                 a larger or less contended run may succeed",
            )?;
            write_diagnostic(formatter, "; tool output", diagnostic.as_ref())
        }
        CompilerCause::DiagnosticLimit { limit, observed, diagnostic } => {
            write!(
                formatter,
                "the native compiler wrote {observed} bytes of diagnostics, over the {limit} byte limit"
            )?;
            write_diagnostic(formatter, "; first output", diagnostic.as_ref())
        }
        CompilerCause::Lowering(_) => formatter.write_str(
            "lowering refused it: the source uses a construct its language's lowering \
             does not represent yet",
        ),
        CompilerCause::Fragment(phase) => write!(
            formatter,
            "compact IR construction failed during {}",
            fragment_phase(*phase)
        ),
        CompilerCause::FragmentFailure(_) => {
            formatter.write_str("compact IR construction refused the lowered facts")
        }
    }
}

fn write_native_work(formatter: &mut fmt::Formatter<'_>, work: &NativeWorkCause) -> fmt::Result {
    match work {
        NativeWorkCause::Directory { phase, cause } => write!(
            formatter,
            "the native work directory could not be {}: {}",
            work_phase(*phase),
            directory_cause(*cause)
        ),
        NativeWorkCause::Artifact { action, cause, .. } => write!(
            formatter,
            "a native work file could not be {}: {}",
            artifact_action(*action),
            match cause {
                backend_library::interface::NativeArtifactCause::Io(fact) => io_fact(*fact),
                backend_library::interface::NativeArtifactCause::InvalidText(_) =>
                    "the name was not valid UTF-8".to_owned(),
            }
        ),
        NativeWorkCause::Primary(_) | NativeWorkCause::PrimaryAndCleanup { .. } => {
            formatter.write_str("the native compiler process failed")
        }
    }
}

fn write_publication(formatter: &mut fmt::Formatter<'_>, cause: PublicationCause) -> fmt::Result {
    match cause {
        PublicationCause::CancelledBeforeStorage
        | PublicationCause::CancelledBeforeAdmission
        | PublicationCause::CancelledAfterAdmission => {
            formatter.write_str("publication was cancelled")
        }
        PublicationCause::AdmissionFull => {
            formatter.write_str("the publisher is full; retry when indexing has drained")
        }
        PublicationCause::AdmissionClosed => formatter.write_str("the publisher has stopped"),
        PublicationCause::Rejected(phase) => write!(
            formatter,
            "durable publication rejected the {}",
            publication_phase(phase)
        ),
        PublicationCause::StableGenerationMismatch => {
            formatter.write_str("the published generation did not match its binding")
        }
    }
}

fn write_diagnostic(
    formatter: &mut fmt::Formatter<'_>,
    lead: &str,
    diagnostic: Option<&CompilerDiagnostic>,
) -> fmt::Result {
    let Some(diagnostic) = diagnostic else {
        return Ok(());
    };
    let retained = diagnostic.bytes.get(..diagnostic.byte_len).unwrap_or_default();
    write_diagnostic_bytes(formatter, lead, retained)?;
    if diagnostic.truncated {
        formatter.write_str(" [truncated]")?;
    }
    Ok(())
}

/// Repeats a bounded diagnostic prefix as single-line text.
fn write_diagnostic_bytes(
    formatter: &mut fmt::Formatter<'_>,
    lead: &str,
    bytes: &[u8],
) -> fmt::Result {
    let text = String::from_utf8_lossy(bytes);
    let mut line = String::new();
    for character in text.trim().chars() {
        if line.len() + character.len_utf8() > MAX_DIAGNOSTIC_TEXT_BYTES {
            line.push_str("...");
            break;
        }
        line.push(if character.is_control() { ' ' } else { character });
    }
    if line.is_empty() {
        return Ok(());
    }
    write!(formatter, "{lead}{line}")
}

fn io_fact(fact: NativeIoFact) -> String {
    let mut text = fact.kind.to_string();
    if let Some(code) = fact.raw_os_code {
        let _ = write!(text, " (os error {code})");
    }
    text
}

const fn package_phase(phase: PackageCompilePhase) -> &'static str {
    match phase {
        PackageCompilePhase::Locate => "located",
        PackageCompilePhase::EnterSource => "read",
        PackageCompilePhase::Authority => "analyzed by its language authority",
        PackageCompilePhase::Lower => "lowered",
        PackageCompilePhase::Publish => "published",
        PackageCompilePhase::Reopen => "reopened after publication",
        PackageCompilePhase::Discover => "prepared for search",
        PackageCompilePhase::Render => "rendered",
    }
}

fn runtime_cause(cause: &CompilerRuntimeCause) -> String {
    match cause {
        CompilerRuntimeCause::RequestInFlight => "another request holds this package".to_owned(),
        CompilerRuntimeCause::CommandQueueFull { capacity } => {
            format!("its command queue is full ({capacity} slots)")
        }
        CompilerRuntimeCause::RequestAdmissionFull { capacity } => {
            format!("it already admitted {capacity} requests")
        }
        CompilerRuntimeCause::LaneQueueFull { capacity } => {
            format!("the compiler lane is full ({capacity} jobs)")
        }
        CompilerRuntimeCause::StagedOutputBudgetExceeded {
            requested_bytes,
            available_bytes,
            capacity_bytes,
        } => format!(
            "the staged output needs {requested_bytes} bytes but {available_bytes} of {capacity_bytes} are available"
        ),
        CompilerRuntimeCause::StagedOutputReservationOverflow => {
            "the staged output size overflowed".to_owned()
        }
        CompilerRuntimeCause::QueueFull => "its queue is full".to_owned(),
        CompilerRuntimeCause::RequestOwnerStopped => "it stopped before admitting the request".to_owned(),
        CompilerRuntimeCause::ResponseOwnerStopped => "it stopped before answering".to_owned(),
        CompilerRuntimeCause::ToolchainProbeTimeout => "the toolchain probe timed out".to_owned(),
        CompilerRuntimeCause::NativePythonProducerAdmission { failure } => {
            format!("the native Python checker failed admission: {}", failure.detail())
        }
        CompilerRuntimeCause::RequestCancelled => "the request was cancelled".to_owned(),
        CompilerRuntimeCause::WorkerPanic(_) => "its worker panicked".to_owned(),
    }
}

const fn language_name(language: Language) -> &'static str {
    match language {
        Language::Rust => "Rust",
        Language::TypeScript => "TypeScript",
        Language::Python => "Python",
        Language::Go => "Go",
        Language::Java => "Java",
        Language::CSharp => "C#",
        Language::Clang => "C/C++",
    }
}

const fn tool_name(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::Rustc => "rustc",
        NativeTool::Clang => "clang",
        NativeTool::Python => "python",
        NativeTool::TypeScriptCompiler => "tsc",
        NativeTool::GoCompiler => "go",
        NativeTool::JavaCompiler => "javac",
        NativeTool::CSharpCompiler => "dotnet",
    }
}

const fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Parse => "parse",
        Stage::LowerIr => "lowering",
    }
}

const fn authority_phase(phase: AuthorityPhase) -> &'static str {
    match phase {
        AuthorityPhase::Open => "while opening the project",
        AuthorityPhase::Parse => "while parsing",
        AuthorityPhase::Resolve => "while resolving symbols and imports",
        AuthorityPhase::TypeCheck => "while type checking",
        AuthorityPhase::Project => "while projecting facts",
    }
}

const fn authority_class(class: AuthorityDiagnosticClass) -> &'static str {
    match class {
        AuthorityDiagnosticClass::Syntax => "syntax",
        AuthorityDiagnosticClass::Binding => "binding",
        AuthorityDiagnosticClass::SourceScope => "outside the active source scope",
        AuthorityDiagnosticClass::Type => "types",
        AuthorityDiagnosticClass::Authority => "the authority tool itself",
        AuthorityDiagnosticClass::Projection => "canonical projection",
    }
}

const fn fragment_phase(phase: FragmentCause) -> &'static str {
    match phase {
        FragmentCause::Prepare => "preparation",
        FragmentCause::Write => "output writing",
        FragmentCause::Validate => "validation",
    }
}

const fn work_phase(phase: NativeWorkPhase) -> &'static str {
    match phase {
        NativeWorkPhase::Prepare => "prepared",
        NativeWorkPhase::Cleanup => "cleaned",
    }
}

fn directory_cause(cause: NativeDirectoryCause) -> String {
    match cause {
        NativeDirectoryCause::RelativeDirectory => "its path is relative".to_owned(),
        NativeDirectoryCause::NotEmpty => "it is not empty".to_owned(),
        NativeDirectoryCause::InspectIo(fact) => io_fact(fact),
    }
}

const fn artifact_action(action: NativeArtifactAction) -> &'static str {
    match action {
        NativeArtifactAction::ResolveText => "named",
        NativeArtifactAction::Write => "written",
        NativeArtifactAction::CreateDirectory => "created",
        NativeArtifactAction::Remove => "removed",
    }
}

const fn publication_phase(phase: PublicationPhase) -> &'static str {
    match phase {
        PublicationPhase::Canonical => "canonical package",
        PublicationPhase::Manifest => "manifest",
        PublicationPhase::Fragment => "compact fragment",
        PublicationPhase::SemanticImage => "semantic image",
        PublicationPhase::Generation => "generation",
        PublicationPhase::Binding => "generation binding",
        PublicationPhase::Durable => "durable publisher record",
        PublicationPhase::Reopen => "reopened generation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::interface::{CompilerAttempt, SourceAuthority};
    use backend_version::{ContentId, SourceFactDomain};

    fn attempted() -> CompilerAttempt {
        CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"package main"),
                byte_len: 12,
            },
            recipe: ContentId::from_canonical_bytes(b"recipe"),
        }
    }

    fn text(terminal: &CompilerTerminal) -> String {
        TerminalText::new("encode_test.go", terminal).to_string()
    }

    #[test]
    fn a_deadline_is_explained_without_rust_debug_structure() {
        let terminal = CompilerTerminal::Compile {
            attempted: attempted(),
            cause: CompilerCause::DeadlineExceeded { diagnostic: None },
        };
        let rendered = text(&terminal);
        assert!(rendered.contains("compile deadline passed"), "{rendered}");
        for leaked in ["Compile {", "CompilerAttempt", "ContentId", "SourceAuthority", "DeadlineExceeded"] {
            assert!(!rendered.contains(leaked), "{leaked} leaked into {rendered}");
        }
    }

    #[test]
    fn a_native_rejection_keeps_its_exit_status_and_single_line_diagnostic() {
        let terminal = CompilerTerminal::Compile {
            attempted: attempted(),
            cause: CompilerCause::NativeRejected {
                code: Some(2),
                diagnostic: CompilerDiagnostic::from_native(b"bad.go:3:1: syntax error\n  unexpected }", 36, false),
            },
        };
        assert_eq!(
            text(&terminal),
            "the native compiler rejected the source (exit status 2): bad.go:3:1: syntax error   unexpected }"
        );
    }

    #[test]
    fn a_runtime_terminal_names_the_language_and_the_closed_cause() {
        let terminal = CompilerTerminal::Runtime {
            language: Language::Python,
            stage: Stage::LowerIr,
            target: None,
            cause: CompilerRuntimeCause::NativePythonProducerAdmission {
                failure: backend_library::interface::PythonAuthorityFailureKind::WorkspaceIoFailed,
            },
        };
        assert_eq!(
            text(&terminal),
            "the Python compiler owner could not finish the request: the native Python checker failed admission: Python checker workspace I/O failed"
        );
    }

    #[test]
    fn csharp_unavailable_names_both_authorities_without_guessing_which_is_missing() {
        let terminal = CompilerTerminal::Unavailable {
            language: Language::CSharp,
            stage: Stage::LowerIr,
        };
        let rendered = text(&terminal);
        assert!(
            rendered.contains("C# semantic authority is unavailable"),
            "{rendered}"
        );
        assert!(rendered.contains("NUDOX_DOTNET"), "{rendered}");
        assert!(rendered.contains("NUDOX_ROSLYN_HELPER"), "{rendered}");
        assert!(!rendered.contains("in this build"), "{rendered}");
    }

    #[test]
    fn non_csharp_unavailable_wording_is_unchanged() {
        for (language, name) in [(Language::Go, "Go"), (Language::Python, "Python")] {
            let terminal = CompilerTerminal::Unavailable {
                language,
                stage: Stage::LowerIr,
            };
            let rendered = text(&terminal);
            assert_eq!(
                rendered,
                format!("no {name} compiler is available for the lowering stage in this build")
            );
            assert!(!rendered.contains("NUDOX_ROSLYN_HELPER"), "{rendered}");
        }
    }

    #[test]
    fn csharp_missing_dotnet_requirement_keeps_its_exact_configuration_variable() {
        use backend_library::interface::{
            CompilerToolFailure, CompilerToolIssue, CompilerToolRequirement,
        };

        let terminal = CompilerTerminal::RequiredTool {
            source: attempted().source,
            language: Language::CSharp,
            stage: Stage::LowerIr,
            issue: CompilerToolIssue {
                requirement: CompilerToolRequirement::Native(NativeTool::CSharpCompiler),
                failure: CompilerToolFailure::Missing,
            },
        };
        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("Library.cs", &terminal)
                .expect("valid package failure")
                .expect("required native tool is projected");
        assert_eq!(
            failure.required_configuration_variable(),
            Some("NUDOX_DOTNET")
        );
        assert_eq!(
            failure.required_native_tool(),
            Some(backend_library::CompilerNativeToolFact::CSharpCompiler)
        );
        assert!(failure.detail().contains("dotnet is required"));
        assert!(failure.detail().contains("NUDOX_DOTNET"));
        assert!(!failure.detail().contains("NUDOX_ROSLYN_HELPER"));
    }

    #[test]
    fn a_cancelled_package_names_the_phase_it_stopped_in() {
        let terminal = CompilerTerminal::PackageCancelled {
            target: ContentId::from_canonical_bytes(b"target"),
            phase: PackageCompilePhase::Authority,
        };
        assert_eq!(
            text(&terminal),
            "the request was cancelled while the package was being analyzed by its language authority"
        );
    }

    #[test]
    fn a_lowering_gap_reads_as_a_refusal_of_that_construct() {
        let terminal = CompilerTerminal::Compile {
            attempted: attempted(),
            cause: CompilerCause::Fragment(FragmentCause::Validate),
        };
        assert_eq!(text(&terminal), "compact IR construction failed during validation");
    }
}
