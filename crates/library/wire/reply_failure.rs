use super::{EmptyWire, TextWire};
use crate::{CommandFailure, ViewRevision, decode_id, encode_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandFailureWire {
    NotFound(EmptyWire),
    WrongBasis(WrongBasisWire),
    InvalidQuery(TextWire),
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
        CommandFailureWire::CursorMismatch(_) => CommandFailure::CursorMismatch,
        CommandFailureWire::IncoherentView(value) => CommandFailure::IncoherentView(value.text),
        CommandFailureWire::SequenceOverflow(_) => CommandFailure::SequenceOverflow,
        CommandFailureWire::MutationRequiresOwner(_) => CommandFailure::MutationRequiresOwner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_failure_round_trips() -> Result<(), String> {
        let failures = [
            CommandFailure::NotFound,
            CommandFailure::WrongBasis {
                expected: ViewRevision::from_bytes([1; backend_version::ID_BYTES]),
                observed: ViewRevision::from_bytes([2; backend_version::ID_BYTES]),
            },
            CommandFailure::InvalidQuery("invalid".to_owned()),
            CommandFailure::CursorMismatch,
            CommandFailure::IncoherentView("incoherent".to_owned()),
            CommandFailure::SequenceOverflow,
            CommandFailure::MutationRequiresOwner,
        ];
        for failure in failures {
            let decoded = command_failure_from_wire(command_failure_to_wire(&failure))?;
            assert_eq!(decoded, failure);
        }
        Ok(())
    }
}
