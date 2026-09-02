//! Defines native typescript behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native typescript invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Source coordinates for TypeScript facts come from this module's typed coordinate boundary:
//! every span fact is a `Utf8Span` byte unit validated against the exact source, and raw
//! `(offset, length)` primitive pairs or cross-unit values can never cross that boundary.
use std::{path::Path, process::Command};

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

pub(crate) mod coords;

const SOURCE_FILE: &str = "compiler-probe.ts";
const WORK_DIRECTORY: &str = "typescript";

/// Native `tsc` syntax and type-check admission over one owned source file.
pub(super) struct TypeScriptFrontend;

impl NativeFrontend for TypeScriptFrontend {
    fn prepare(native_work: &Path, source: &[u8]) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::TypeScriptWork)?;
        write_artifact(
            &work.join(SOURCE_FILE),
            source,
            NativeArtifactRole::TypeScriptSource,
        )
    }

    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "--noEmit",
                "--pretty",
                "false",
                "--target",
                "ES2022",
                "--module",
                "ESNext",
                SOURCE_FILE,
            ])
            .current_dir(native_work.join(WORK_DIRECTORY))
            .env_clear();
        command
    }

    fn source_via_stdin() -> bool {
        false
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_directory_if_present(
            native_work.join(WORK_DIRECTORY),
            NativeArtifactRole::TypeScriptWork,
        )
    }
}

#[cfg(test)]
mod source_coordinate_contract {
    use super::coords::{
        AuthorityOrigin, SourceSpan, SourceSpanError, Utf8Span, Utf16Offset, utf16_span_to_utf8,
    };

    #[derive(Debug, thiserror::Error)]
    enum CoordinateContract {
        #[error("the frontend coordinate boundary unexpectedly accepted the span as {0:?}")]
        Accepted(Utf8Span),
        #[error(
            "the frontend coordinate boundary rejected with {0:?}, not an invalid UTF-16 boundary"
        )]
        RejectedOtherwise(SourceSpanError),
    }

    #[test]
    fn a_utf16_span_selects_exact_probe_bytes_through_the_frontend_boundary()
    -> Result<(), SourceSpanError> {
        let probe_source = "export const café: string = \"grüße\";";
        let span = SourceSpan {
            origin: AuthorityOrigin::TszSemantic,
            start: Utf16Offset { units: 13 },
            end: Utf16Offset { units: 17 },
        };

        let actual = utf16_span_to_utf8(probe_source.as_bytes(), span)?;

        assert_eq!(actual, Utf8Span { start: 13, end: 18 });
        assert_eq!(
            &probe_source.as_bytes()[actual.start..actual.end],
            "café".as_bytes()
        );
        Ok(())
    }

    #[test]
    fn a_coordinate_inside_a_surrogate_pair_never_selects_probe_bytes()
    -> Result<(), CoordinateContract> {
        let probe_source = "export const 😀x: string = \"x\";";
        let span = SourceSpan {
            origin: AuthorityOrigin::OxcSyntax,
            start: Utf16Offset { units: 14 },
            end: Utf16Offset { units: 15 },
        };

        match utf16_span_to_utf8(probe_source.as_bytes(), span) {
            Err(SourceSpanError::InvalidUtf16Boundary {
                span: retained,
                offset: Utf16Offset { units: 14 },
            }) => {
                assert_eq!(retained, span);
                Ok(())
            }
            Err(otherwise) => Err(CoordinateContract::RejectedOtherwise(otherwise)),
            Ok(accepted) => Err(CoordinateContract::Accepted(accepted)),
        }
    }
}
