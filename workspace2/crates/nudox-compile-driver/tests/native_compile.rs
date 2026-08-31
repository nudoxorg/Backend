use std::{
    env,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use nudox_compile_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    LoweringUnsupported, NativeTool, ResolvedToolchain, ToolchainResolutionError,
    ToolchainSelection, compile,
};
use nudox_compile_vocab::{Language, Stage};
use nudox_ir_format::{EntityKind, PrimitiveType, TypeNode};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestFailure {
    #[error("host does not expose the required {tool:?} executable")]
    MissingHostTool { tool: NativeTool },
    #[error("could not canonicalize the required {tool:?} executable")]
    Canonicalize {
        tool: NativeTool,
        #[source]
        cause: std::io::Error,
    },
    #[error(transparent)]
    Resolve(#[from] ToolchainResolutionError),
    #[error("fixture source length {actual} does not fit the compact source fact")]
    SourceLength {
        actual: usize,
        #[source]
        cause: core::num::TryFromIntError,
    },
    #[error("could not create the caller-owned native work directory")]
    CreateWork(#[source] std::io::Error),
    #[error("could not inspect the caller-owned native work directory")]
    InspectWork(#[source] std::io::Error),
    #[error("the caller-owned native work directory was not clean after native reaping")]
    WorkNotEmpty,
    #[error("the locally reproducible {tool:?} adapter did not compile its valid fixture")]
    Compile { tool: NativeTool },
}

static WORK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct TemporaryWork {
    path: PathBuf,
}

impl TemporaryWork {
    fn create() -> Result<Self, TestFailure> {
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "nudox-compile-work-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(TestFailure::CreateWork)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn assert_empty(&self) -> Result<(), TestFailure> {
        let mut entries = fs::read_dir(&self.path).map_err(TestFailure::InspectWork)?;
        match entries.next() {
            Some(Ok(_entry)) => Err(TestFailure::WorkNotEmpty),
            Some(Err(cause)) => Err(TestFailure::InspectWork(cause)),
            None => Ok(()),
        }
    }
}

impl Drop for TemporaryWork {
    fn drop(&mut self) {
        let _removed = fs::remove_dir(&self.path);
    }
}

fn executable(tool: NativeTool) -> Result<PathBuf, TestFailure> {
    let name = match tool {
        NativeTool::Rustc => "rustc",
        NativeTool::Python => "python3",
        NativeTool::Clang => "clang",
        NativeTool::TypeScriptCompiler
        | NativeTool::GoCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => return Err(TestFailure::MissingHostTool { tool }),
    };
    let paths = env::var_os("PATH").ok_or(TestFailure::MissingHostTool { tool })?;
    for directory in env::split_paths(&paths) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|cause| TestFailure::Canonicalize { tool, cause });
        }
    }
    Err(TestFailure::MissingHostTool { tool })
}

fn resolved<'path>(
    tool: NativeTool,
    path: &'path Path,
) -> Result<ResolvedToolchain<'path>, TestFailure> {
    Ok(ResolvedToolchain::from_version(
        tool,
        path,
        b"native-compile-integration-fixture-v1",
    )?)
}

fn source_length(source: &[u8]) -> Result<u32, TestFailure> {
    u32::try_from(source.len()).map_err(|cause| TestFailure::SourceLength {
        actual: source.len(),
        cause,
    })
}

fn request<'source, 'path, 'cancel>(
    language: Language,
    source: &'source [u8],
    toolchain: ToolchainSelection<'path>,
    cancelled: &'cancel AtomicBool,
    deadline: Instant,
) -> CompileRequest<'source, 'path, 'cancel> {
    CompileRequest {
        language,
        stage: Stage::LowerIr,
        source,
        toolchain,
        control: CompileControl { deadline, cancelled },
    }
}

#[test]
fn native_adapters_parse_real_source_before_lending_compact_ir() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::Rust,
            NativeTool::Rustc,
            b"pub const RUST_VALID: &str = \"yes\";".as_slice(),
            b"RUST_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"PYTHON_VALID = \"yes\"\n".as_slice(),
            b"PYTHON_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"const char *clang_valid = \"yes\";".as_slice(),
            b"clang_valid".as_slice(),
            PrimitiveType::String,
        ),
    ];
    for (language, tool, source, expected_atom, expected_type) in cases {
        let source_length = source_length(source)?;
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0xa5; 4_096];
        let mut output = [0xa5; 512];
        let output_pointer = output.as_ptr();
        let fragment_len = match compile(
            request(
                language,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
                &cancelled,
                Instant::now() + Duration::from_secs(5),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ) {
            Ok(compiled) => {
                assert_eq!(compiled.recipe.language, language);
                assert_eq!(compiled.recipe.stage, Stage::LowerIr);
                assert_eq!(compiled.recipe.tool, tool);
                assert_eq!(compiled.source.byte_len, source_length);
                assert!(core::ptr::eq(compiled.fragment.as_ref().as_ptr(), output_pointer));
                assert_eq!(compiled.fragment.entities().count(), 1);
                assert_eq!(compiled.fragment.type_nodes().count(), 1);
                assert_eq!(
                    compiled.fragment.atoms().next().map(|atom| atom.bytes),
                    Some(expected_atom)
                );
                assert!(compiled
                    .fragment
                    .type_nodes()
                    .eq([TypeNode::Primitive(expected_type)]));
                compiled.fragment.as_ref().len()
            }
            Err(_) => return Err(TestFailure::Compile { tool }),
        };
        native_work.assert_empty()?;
        assert!(output[fragment_len..].iter().all(|byte| *byte == 0xa5));
    }
    Ok(())
}

#[test]
fn native_rejection_retains_recipe_source_and_bounded_diagnostic() -> Result<(), TestFailure> {
    let source = b"pub const = ;";
    let source_length = source_length(source)?;
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4_096];
    let mut output = [0xa5; 512];
    match compile(
        request(
            Language::Rust,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::NativeRejected {
            source_identity,
            recipe,
            diagnostic,
            ..
        }) => {
            assert_eq!(recipe.language, Language::Rust);
            assert_eq!(recipe.stage, Stage::LowerIr);
            assert_eq!(recipe.tool, NativeTool::Rustc);
            assert_eq!(source_identity.byte_len, source_length);
            assert!(!diagnostic.bytes.is_empty());
            assert!(!diagnostic.truncated);
        }
        _ => return Err(TestFailure::Compile { tool: NativeTool::Rustc }),
    }
    native_work.assert_empty()?;
    assert!(output.iter().all(|byte| *byte == 0xa5));
    Ok(())
}

#[test]
fn unavailable_tooling_is_an_explicit_typed_terminal() -> Result<(), TestFailure> {
    let source = b"export const unavailable: number = 1;";
    let source_length = source_length(source)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 512];
    assert!(matches!(
        compile(
            request(
                Language::TypeScript,
                source,
                ToolchainSelection::ExplicitlyUnavailable {
                    tool: NativeTool::TypeScriptCompiler,
                },
                &cancelled,
                Instant::now() + Duration::from_secs(1),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ),
        Err(CompileFailure::ToolingUnavailable {
            source_identity,
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            tool: NativeTool::TypeScriptCompiler,
        }) if source_identity.byte_len == source_length
    ));
    native_work.assert_empty()?;
    assert!(output.iter().all(|byte| *byte == 0xa5));
    Ok(())
}

#[test]
fn native_lowering_rejects_an_unavailable_selection_before_any_tool_spawn()
-> Result<(), TestFailure> {
    let source = b"pub const alpha: bool = true;";
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 512];
    assert!(matches!(
        compile(
            request(
                Language::Rust,
                source,
                ToolchainSelection::ExplicitlyUnavailable {
                    tool: NativeTool::Rustc,
                },
                &cancelled,
                Instant::now() + Duration::from_secs(1),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ),
        Err(CompileFailure::ToolchainSelectionMismatch {
            language: Language::Rust,
            stage: Stage::LowerIr,
            selected: NativeTool::Rustc,
            ..
        })
    ));
    native_work.assert_empty()?;
    assert!(output.iter().all(|byte| *byte == 0xa5));
    Ok(())
}

#[test]
fn unsupported_rust_outer_type_cannot_borrow_an_inner_bool_annotation()
-> Result<(), TestFailure> {
    let source = b"pub const alpha: u64 = { const INNER: bool = true; 1 };";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4_096];
    let mut output = [0xa5; 512];
    assert!(matches!(
        compile(
            request(
                Language::Rust,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
                &cancelled,
                Instant::now() + Duration::from_secs(5),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ),
        Err(CompileFailure::LoweringUnsupported {
            cause: LoweringUnsupported::RustConstantType,
            ..
        })
    ));
    native_work.assert_empty()?;
    assert!(output.iter().all(|byte| *byte == 0xa5));
    Ok(())
}

#[test]
fn rust_declaration_atoms_kinds_and_types_reject_source_digest_only_lowering()
-> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1;";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0; 4_096];
    let mut bravo_diagnostic = [0; 4_096];
    let mut alpha_output = [0; 512];
    let mut bravo_output = [0; 512];
    let alpha = compile(
        request(
            Language::Rust,
            alpha_source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut alpha_diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut alpha_output,
        },
    )
    .map_err(|_| TestFailure::Compile { tool: NativeTool::Rustc })?;
    native_work.assert_empty()?;
    let bravo = compile(
        request(
            Language::Rust,
            bravo_source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut bravo_diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut bravo_output,
        },
    )
    .map_err(|_| TestFailure::Compile { tool: NativeTool::Rustc })?;
    native_work.assert_empty()?;

    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    assert_ne!(alpha.source.identity, bravo.source.identity);
    assert_eq!(
        alpha.fragment.entities().next().map(|entity| entity.kind),
        Some(EntityKind::Constant)
    );
    assert_eq!(
        alpha.fragment.atoms().next().map(|atom| atom.bytes),
        Some(&b"alpha"[..])
    );
    assert_eq!(
        bravo.fragment.atoms().next().map(|atom| atom.bytes),
        Some(&b"bravo"[..])
    );
    assert!(alpha
        .fragment
        .type_nodes()
        .eq([TypeNode::Primitive(PrimitiveType::Bool)]));
    assert!(bravo
        .fragment
        .type_nodes()
        .eq([TypeNode::Primitive(PrimitiveType::I32)]));
    Ok(())
}

#[test]
fn relative_toolchain_path_is_never_a_path_lookup_capability() {
    assert_eq!(
        ResolvedToolchain::from_version(NativeTool::Rustc, Path::new("rustc"), b"fixture"),
        Err(ToolchainResolutionError::RelativeExecutable)
    );
}

#[cfg(unix)]
mod bounded_native {
    use std::{
        env,
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    use nudox_compile_driver::{
        CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
        NativeTool, NativeWorkError, NativeWorkPrimary, ResolvedToolchain, ToolchainSelection,
        compile,
    };
    use nudox_compile_vocab::{FrontendError, Language, Stage};
    use thiserror::Error;

    use super::TemporaryWork;

    static SCRIPT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, Error)]
    enum ScriptFailure {
        #[error("could not create the bounded-native test executable")]
        Create(#[source] std::io::Error),
        #[error("could not write the bounded-native test executable")]
        Write(#[source] std::io::Error),
        #[error("could not make the bounded-native test executable runnable")]
        Permissions(#[source] std::io::Error),
        #[error(transparent)]
        Resolve(#[from] nudox_compile_driver::ToolchainResolutionError),
        #[error(transparent)]
        Work(#[from] super::TestFailure),
        #[error("the hostile native helper returned a terminal other than the one under test")]
        UnexpectedTerminal,
        #[error("could not remove the exact hostile helper artifact after its cleanup test")]
        RemoveHostileArtifact(#[source] std::io::Error),
    }

    struct TemporaryExecutable {
        path: PathBuf,
    }

    impl Drop for TemporaryExecutable {
        fn drop(&mut self) {
            let _removed = fs::remove_file(&self.path);
        }
    }

    fn script(body: &[u8]) -> Result<TemporaryExecutable, ScriptFailure> {
        let sequence = SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "nudox-compile-driver-{}-{sequence}.sh",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(ScriptFailure::Create)?;
        file.write_all(body).map_err(ScriptFailure::Write)?;
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(ScriptFailure::Permissions)?;
        Ok(TemporaryExecutable { path })
    }

    fn request<'source, 'path, 'cancel>(
        source: &'source [u8],
        toolchain: ToolchainSelection<'path>,
        cancelled: &'cancel AtomicBool,
        deadline: Instant,
    ) -> CompileRequest<'source, 'path, 'cancel> {
        CompileRequest {
            language: Language::Rust,
            stage: Stage::LowerIr,
            source,
            toolchain,
            control: CompileControl { deadline, cancelled },
        }
    }

    #[test]
    fn nonreading_never_exit_tool_is_killed_without_blocking_the_deadline_owner()
    -> Result<(), ScriptFailure> {
        let executable = script(b"#!/bin/sh\nwhile :; do :; done\n")?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"nonreading-fixture",
        )?;
        let source = [b'x'; 131_072];
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 128];
        let mut output = [0; 512];
        assert!(matches!(
            compile(
                request(
                    &source,
                    ToolchainSelection::ResolvedNative(toolchain),
                    &cancelled,
                    Instant::now() + Duration::from_millis(300),
                ),
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: native_work.path(),
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ),
            Err(CompileFailure::DeadlineExceeded { .. })
        ));
        native_work.assert_empty()?;
        Ok(())
    }

    #[test]
    fn stdin_closed_then_never_exit_retains_the_input_cause_and_reaps_the_child()
    -> Result<(), ScriptFailure> {
        let executable = script(b"#!/bin/sh\nexec 0<&-\nwhile :; do :; done\n")?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"stdin-closed-fixture",
        )?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let source = [b'x'; 131_072];
        let mut diagnostic = [0; 128];
        let mut output = [0xa5; 512];
        assert!(matches!(
            compile(
                request(
                    &source,
                    ToolchainSelection::ResolvedNative(toolchain),
                    &cancelled,
                    Instant::now() + Duration::from_secs(1),
                ),
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: native_work.path(),
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ),
            Err(CompileFailure::ToolInput { .. })
        ));
        native_work.assert_empty()?;
        assert!(output.iter().all(|byte| *byte == 0xa5));
        Ok(())
    }

    #[test]
    fn pre_cancelled_request_never_starts_the_marker_tool()
    -> Result<(), ScriptFailure> {
        let executable = script(b"#!/bin/sh\nprintf x > started\nwhile :; do :; done\n")?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"pre-cancelled-fixture",
        )?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(true);
        let mut diagnostic = [0; 128];
        let mut output = [0xa5; 512];
        assert!(matches!(
            compile(
                request(
                    b"pub const alpha: bool = true;",
                    ToolchainSelection::ResolvedNative(toolchain),
                    &cancelled,
                    Instant::now() + Duration::from_secs(1),
                ),
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: native_work.path(),
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ),
            Err(CompileFailure::Cancelled { diagnostic, .. }) if diagnostic.bytes.is_empty()
        ));
        native_work.assert_empty()?;
        assert!(output.iter().all(|byte| *byte == 0xa5));
        Ok(())
    }

    #[test]
    fn parse_stage_is_a_pre_spawn_typed_terminal_and_never_lends_ir()
    -> Result<(), ScriptFailure> {
        let executable = script(b"#!/bin/sh\nprintf x > started\nwhile :; do :; done\n")?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"parse-stage-fixture",
        )?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 128];
        let mut output = [0xa5; 512];
        assert!(matches!(
            compile(
                CompileRequest {
                    language: Language::Rust,
                    stage: Stage::Parse,
                    source: b"pub const alpha: bool = true;",
                    toolchain: ToolchainSelection::ResolvedNative(toolchain),
                    control: CompileControl {
                        deadline: Instant::now() + Duration::from_secs(1),
                        cancelled: &cancelled,
                    },
                },
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: native_work.path(),
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ),
            Err(CompileFailure::UnsupportedStage {
                language: Language::Rust,
                stage: Stage::Parse,
                cause: FrontendError::UnsupportedStage {
                    language: Language::Rust,
                    stage: Stage::Parse,
                },
                ..
            })
        ));
        native_work.assert_empty()?;
        assert!(output.iter().all(|byte| *byte == 0xa5));
        Ok(())
    }

    #[test]
    fn cleanup_failure_retains_the_exact_native_rejection_terminal()
    -> Result<(), ScriptFailure> {
        let executable = script(
            b"#!/bin/sh\nIFS= read -r ignored\nprintf x > foreign\nprintf rejected >&2\nexit 1\n",
        )?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"cleanup-failure-fixture",
        )?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 128];
        let mut output = [0xa5; 512];
        assert!(matches!(
            compile(
                request(
                    b"fixture\n",
                    ToolchainSelection::ResolvedNative(toolchain),
                    &cancelled,
                    Instant::now() + Duration::from_secs(1),
                ),
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: native_work.path(),
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ),
            Err(CompileFailure::NativeWorkCleanup {
                primary: NativeWorkPrimary::NativeRejected { diagnostic, .. },
                cleanup: NativeWorkError::NotEmpty,
                ..
            }) if diagnostic.bytes == b"rejected"
        ));
        fs::remove_file(native_work.path().join("foreign"))
            .map_err(ScriptFailure::RemoveHostileArtifact)?;
        native_work.assert_empty()?;
        assert!(output.iter().all(|byte| *byte == 0xa5));
        Ok(())
    }

    #[test]
    fn noisy_tool_exceeds_the_bounded_diagnostic_lease_before_a_compile_terminal()
    -> Result<(), ScriptFailure> {
        let executable = script(
            b"#!/bin/sh\nwhile :; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' >&2; done\n",
        )?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            Path::new(&executable.path),
            b"noisy-fixture",
        )?;
        let cancelled = AtomicBool::new(false);
        let native_work = TemporaryWork::create()?;
        let mut diagnostic = [0; 32];
        let mut output = [0; 512];
        match compile(
            request(
                b"pub const alpha: bool = true;",
                ToolchainSelection::ResolvedNative(toolchain),
                &cancelled,
                Instant::now() + Duration::from_secs(1),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ) {
            Err(CompileFailure::DiagnosticLimit {
                limit,
                observed,
                diagnostic,
                ..
            }) => {
                assert_eq!(limit, 32);
                assert!(observed > limit);
                assert_eq!(diagnostic.bytes.len(), limit);
                assert!(diagnostic.truncated);
            }
            _ => return Err(ScriptFailure::UnexpectedTerminal),
        }
        native_work.assert_empty()?;
        Ok(())
    }
}
