//! Layout and allocation contracts for cold compiler diagnostics.

use std::mem::size_of;

use allocation_counter::{AllocationInfo, measure};
use nudox_compile_vocab::{Language, Stage};
use wave_application_core::{
    ApplicationInput, ApplicationService, CompilerCapability, CompilerDiagnostic, CompilerRequest,
    CompilerTerminal, CorrelationId, MAX_NATIVE_DIAGNOSTIC_BYTES, MAX_NATIVE_WORKER_PANIC_BYTES,
    UnavailableCompiler,
};

#[derive(Debug, Eq, PartialEq)]
enum DiagnosticAllocationTestError {
    DiagnosticLayout {
        actual: usize,
        expected: usize,
    },
    TerminalLayout {
        actual: usize,
        maximum: usize,
    },
    ReplyLayout {
        actual: usize,
        maximum: usize,
    },
    SuccessAllocation {
        observed: AllocationInfo,
    },
    SuccessDiagnostic,
    UnavailableAllocation {
        observed: AllocationInfo,
    },
    UnavailableTerminal {
        observed: Box<CompilerTerminal>,
    },
    DiagnosticAllocation {
        observed: AllocationInfo,
    },
    DiagnosticDeallocation {
        observed: AllocationInfo,
    },
    DiagnosticFacts {
        byte_len: usize,
        observed: usize,
        truncated: bool,
    },
}

#[test]
fn compiler_diagnostics_are_one_cold_allocation_and_clean_replies_stay_compact()
-> Result<(), DiagnosticAllocationTestError> {
    exact_layout()?;
    successful_health_is_allocation_free()?;
    unavailable_compiler_is_allocation_free()?;
    emitted_native_diagnostic_has_one_owner()
}

fn successful_health_is_allocation_free() -> Result<(), DiagnosticAllocationTestError> {
    let mut service = ApplicationService::new();
    let mut reply = None;
    let success_allocations = measure(|| {
        reply = Some(service.execute(&ApplicationInput::Health {
            correlation: CorrelationId(1),
        }));
    });
    if success_allocations != AllocationInfo::default() {
        return Err(DiagnosticAllocationTestError::SuccessAllocation {
            observed: success_allocations,
        });
    }
    let Some(reply) = reply else {
        return Err(DiagnosticAllocationTestError::SuccessDiagnostic);
    };
    if reply.diagnostic.is_some() {
        return Err(DiagnosticAllocationTestError::SuccessDiagnostic);
    }
    drop(reply);
    Ok(())
}

fn unavailable_compiler_is_allocation_free() -> Result<(), DiagnosticAllocationTestError> {
    let mut unavailable = UnavailableCompiler;
    let mut unavailable_terminal = None;
    let unavailable_allocations = measure(|| {
        unavailable_terminal = unavailable
            .generate(CompilerRequest {
                language: Language::Rust,
                stage: Stage::LowerIr,
                source: "pub const READY: i32 = 1;",
            })
            .err();
    });
    if unavailable_allocations != AllocationInfo::default() {
        return Err(DiagnosticAllocationTestError::UnavailableAllocation {
            observed: unavailable_allocations,
        });
    }
    match unavailable_terminal {
        Some(CompilerTerminal::Unavailable {
            language: Language::Rust,
            stage: Stage::LowerIr,
        }) => {}
        Some(observed) => {
            return Err(DiagnosticAllocationTestError::UnavailableTerminal {
                observed: Box::new(observed),
            });
        }
        None => return Err(DiagnosticAllocationTestError::SuccessDiagnostic),
    }
    Ok(())
}

fn emitted_native_diagnostic_has_one_owner() -> Result<(), DiagnosticAllocationTestError> {
    let mut diagnostic = None;
    let diagnostic_allocations = measure(|| {
        diagnostic = CompilerDiagnostic::from_native(b"native rejected input", 21, false);
    });
    if diagnostic_allocations.count_total != 1
        || diagnostic_allocations.count_current != 1
        || diagnostic_allocations.count_max != 1
    {
        return Err(DiagnosticAllocationTestError::DiagnosticAllocation {
            observed: diagnostic_allocations,
        });
    }
    let Some(diagnostic) = diagnostic else {
        return Err(DiagnosticAllocationTestError::DiagnosticFacts {
            byte_len: 0,
            observed: 0,
            truncated: false,
        });
    };
    if diagnostic.byte_len != 21
        || diagnostic.observed != 21
        || diagnostic.truncated
        || diagnostic.bytes[..diagnostic.byte_len] != *b"native rejected input"
    {
        return Err(DiagnosticAllocationTestError::DiagnosticFacts {
            byte_len: diagnostic.byte_len,
            observed: diagnostic.observed,
            truncated: diagnostic.truncated,
        });
    }
    let diagnostic_deallocations = measure(|| drop(diagnostic));
    if diagnostic_deallocations.count_total != 0
        || diagnostic_deallocations.count_current != -1
        || diagnostic_deallocations.count_max != 0
    {
        return Err(DiagnosticAllocationTestError::DiagnosticDeallocation {
            observed: diagnostic_deallocations,
        });
    }
    Ok(())
}

fn exact_layout() -> Result<(), DiagnosticAllocationTestError> {
    let pointer_bytes = size_of::<usize>();
    let diagnostic_bytes = size_of::<CompilerDiagnostic>();
    if diagnostic_bytes != pointer_bytes {
        return Err(DiagnosticAllocationTestError::DiagnosticLayout {
            actual: diagnostic_bytes,
            expected: pointer_bytes,
        });
    }
    let terminal_bytes = size_of::<CompilerTerminal>();
    if terminal_bytes >= MAX_NATIVE_DIAGNOSTIC_BYTES {
        return Err(DiagnosticAllocationTestError::TerminalLayout {
            actual: terminal_bytes,
            maximum: MAX_NATIVE_DIAGNOSTIC_BYTES - 1,
        });
    }
    let reply_bytes = size_of::<wave_application_core::ApplicationReply>();
    let reply_without_inline_diagnostic_limit =
        2 * MAX_NATIVE_DIAGNOSTIC_BYTES + MAX_NATIVE_WORKER_PANIC_BYTES;
    if reply_bytes >= reply_without_inline_diagnostic_limit {
        return Err(DiagnosticAllocationTestError::ReplyLayout {
            actual: reply_bytes,
            maximum: reply_without_inline_diagnostic_limit - 1,
        });
    }
    Ok(())
}
