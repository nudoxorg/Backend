use super::{EmptyWire, TextWire};
use crate::{CommandFailure, PackageCompilerFailure, ViewRevision, decode_id, encode_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandFailureWire {
    NotFound(EmptyWire),
    WrongBasis(WrongBasisWire),
    InvalidQuery(TextWire),
    CompilerRefused(CompilerRefusedWire),
    CursorMismatch(EmptyWire),
    IncoherentView(TextWire),
    SequenceOverflow(EmptyWire),
    MutationRequiresOwner(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WrongBasisWire {
    expected: String,
    observed: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerRefusedWire {
    detail: TextWire,
    failure: PackageCompilerFailure,
}

pub(crate) fn command_failure_to_wire(failure: &CommandFailure) -> CommandFailureWire {
    match failure {
        CommandFailure::NotFound => CommandFailureWire::NotFound(EmptyWire {}),
        CommandFailure::WrongBasis { expected, observed } => {
            CommandFailureWire::WrongBasis(WrongBasisWire {
                expected: encode_id(expected.as_bytes()),
                observed: encode_id(observed.as_bytes()),
            })
        }
        CommandFailure::InvalidQuery(message) => CommandFailureWire::InvalidQuery(TextWire {
            text: message.clone(),
        }),
        CommandFailure::CompilerRefused { detail, failure } => {
            CommandFailureWire::CompilerRefused(CompilerRefusedWire {
                detail: TextWire {
                    text: detail.clone(),
                },
                failure: failure.clone(),
            })
        }
        CommandFailure::CursorMismatch => CommandFailureWire::CursorMismatch(EmptyWire {}),
        CommandFailure::IncoherentView(message) => CommandFailureWire::IncoherentView(TextWire {
            text: message.clone(),
        }),
        CommandFailure::SequenceOverflow => CommandFailureWire::SequenceOverflow(EmptyWire {}),
        CommandFailure::MutationRequiresOwner => {
            CommandFailureWire::MutationRequiresOwner(EmptyWire {})
        }
    }
}

pub(crate) fn command_failure_from_wire(
    failure: CommandFailureWire,
) -> Result<CommandFailure, String> {
    Ok(match failure {
        CommandFailureWire::NotFound(_) => CommandFailure::NotFound,
        CommandFailureWire::WrongBasis(value) => CommandFailure::WrongBasis {
            expected: ViewRevision::from_bytes(
                decode_id(&value.expected).map_err(|error| error.to_string())?,
            ),
            observed: ViewRevision::from_bytes(
                decode_id(&value.observed).map_err(|error| error.to_string())?,
            ),
        },
        CommandFailureWire::InvalidQuery(value) => CommandFailure::InvalidQuery(value.text),
        CommandFailureWire::CompilerRefused(value) => CommandFailure::CompilerRefused {
            detail: value.detail.text,
            failure: value.failure,
        },
        CommandFailureWire::CursorMismatch(_) => CommandFailure::CursorMismatch,
        CommandFailureWire::IncoherentView(value) => CommandFailure::IncoherentView(value.text),
        CommandFailureWire::SequenceOverflow(_) => CommandFailure::SequenceOverflow,
        CommandFailureWire::MutationRequiresOwner(_) => CommandFailure::MutationRequiresOwner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{CompilerAttempt, CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::FragmentError;
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};

    #[test]
    fn every_command_failure_round_trips() -> Result<(), String> {
        let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source");
        let attempt = CompilerAttempt {
            source: SourceAuthority {
                identity: source_identity,
                byte_len: 6,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
        };
        let fragment_failure = CompilerFragmentFailure::validate(FragmentError::TruncatedHeader {
            required: 32,
            actual: 7,
        });
        let package_failure =
            PackageCompilerFailure::from_fragment_failure("src/lib.ts", attempt, &fragment_failure)
                .map_err(|error| error.to_string())?;
        let failures = [
            CommandFailure::NotFound,
            CommandFailure::WrongBasis {
                expected: ViewRevision::from_bytes([1; backend_version::ID_BYTES]),
                observed: ViewRevision::from_bytes([2; backend_version::ID_BYTES]),
            },
            CommandFailure::InvalidQuery("invalid".to_owned()),
            CommandFailure::CompilerRefused {
                detail: "local semantic compilation failed".to_owned(),
                failure: package_failure,
            },
            CommandFailure::CursorMismatch,
            CommandFailure::IncoherentView("incoherent".to_owned()),
            CommandFailure::SequenceOverflow,
            CommandFailure::MutationRequiresOwner,
        ];
        let display = failures[3].to_string();
        for failure in failures {
            let decoded = command_failure_from_wire(command_failure_to_wire(&failure))?;
            assert_eq!(decoded, failure);
            if let CommandFailure::CompilerRefused { failure, .. } = decoded {
                let value = serde_json::to_value(failure).map_err(|error| error.to_string())?;
                assert_eq!(value["phase"], "validate");
                assert_eq!(value["cause"]["fault"]["facts"]["required"], 32);
                assert_eq!(value["cause"]["fault"]["facts"]["actual"], 7);
            }
        }
        assert!(display.contains("src/lib.ts"));
        assert!(display.contains("validate_truncated_header"));
        assert!(!display.contains("\"required\":32"));
        assert!(!display.contains("content:"));
        assert!(!display.contains("native stdout"));
        Ok(())
    }
}
