//! Observations of one owner-admitted runtime handoff, not semantic corpus availability.

use super::{ProductAdmissionError, ProductText};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Exact owner and installed configuration bound to one accepted operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingRefreshTicket {
    /// Nonzero sequence within this owner lease.
    pub sequence: NonZeroU64,
    /// Actual durable owner epoch.
    pub owner_epoch: u64,
    /// Actual durable owner fence, without truncation.
    pub owner_fence: [u8; 32],
    /// Digest of the complete captured persisted configuration.
    pub configuration: [u8; 32],
}

/// One control action; observer disconnect does not cancel an accepted operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EmbeddingRefreshAction {
    /// Deliberately activate this exact installed configuration.
    Start {
        /// Expected persisted bytes, compared with the owner's actual capture.
        configuration: [u8; 32],
        /// Original owner operation budget, never renewed by polling.
        timeout_seconds: u16,
    },
    /// Observe the exact accepted operation.
    Observe {
        /// Verbatim owner-issued ticket.
        ticket: EmbeddingRefreshTicket,
    },
    /// Explicitly cancel the exact operation; committed receipts remain committed.
    Cancel {
        /// Verbatim owner-issued ticket.
        ticket: EmbeddingRefreshTicket,
    },
}
impl EmbeddingRefreshAction {
    pub(super) fn admit(&self) -> Result<(), ProductAdmissionError> {
        if matches!(self, Self::Start { timeout_seconds, .. } if *timeout_seconds == 0 || *timeout_seconds > 3600)
        {
            return Err(ProductAdmissionError::EmbeddingRefreshShape);
        }
        Ok(())
    }
}

/// Actual progress through the off-control-loop owner handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingRefreshPhase {
    /// Capturing exact configuration and query descriptor.
    Capturing,
    /// Captured; waiting for the service's fresh quiescence check.
    Prepared,
    /// Joining the old query owner and releasing its cache.
    RetiringQuery,
    /// Joining compiler lanes and retiring the old runtime.
    RetiringCompiler,
    /// Admitting the real replacement after old credits have been released.
    Activating,
    /// Installing under the original config witness and owner operation control.
    Committing,
}

/// Closed refusal family. The bounded detail retains the original actionable cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingRefreshFailureKind {
    /// Another accepted mutation or preparation prevents quiescence.
    Busy,
    /// Explicit remote configuration is authoritative.
    ExplicitRemote,
    /// No installed local configuration exists.
    NotConfigured,
    /// Persisted configuration or descriptor no longer matches the capture.
    Configuration,
    /// Original owner lease was replaced or withdrawn.
    Owner,
    /// Query recipe admission or retirement refused.
    Query,
    /// Actual runtime activation refused.
    Activation,
    /// Compiler generation barrier refused.
    Compiler,
    /// Original operation was explicitly cancelled.
    Cancelled,
    /// Original absolute owner operation deadline expired.
    Deadline,
    /// Native worker creation, termination, or completion failed.
    Worker,
}

/// Bounded original refusal, separate from the committed compiler receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingRefreshFailure {
    /// Closed cause family.
    pub kind: EmbeddingRefreshFailureKind,
    /// Original bounded diagnostic, with no invented success or retry outcome.
    pub detail: ProductText,
}

/// Normalization actually validated by the activated runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingRefreshNormalization {
    /// Preserve finite output.
    None,
    /// Require unit L2 output.
    L2,
}

/// Real activated execution identity, including launch policy and all portable facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingRefreshExecution {
    /// Exact model content.
    pub model: [u8; 32],
    /// Configured model version.
    pub model_version: [u8; 32],
    /// Exact tokenizer content.
    pub tokenizer: [u8; 32],
    /// Exact helper and dependency closure.
    pub executable: [u8; 32],
    /// Portable runtime recipe.
    pub recipe: [u8; 32],
    /// Actual launch policy.
    pub launch_configuration: [u8; 32],
    /// Output-affecting portable options.
    pub options_digest: [u8; 32],
    /// Fixed finite vector dimension.
    pub dimension: u32,
    /// Whether the admitted runtime requires unit L2 output.
    pub normalization: EmbeddingRefreshNormalization,
    /// Admitted per-input UTF-8 byte limit.
    pub maximum_text_bytes: u32,
}
impl From<backend_compile::EmbeddingExecutionIdentity> for EmbeddingRefreshExecution {
    fn from(identity: backend_compile::EmbeddingExecutionIdentity) -> Self {
        Self {
            model: identity.model(),
            model_version: identity.model_version(),
            tokenizer: identity.tokenizer(),
            executable: identity.executable(),
            recipe: identity.recipe(),
            launch_configuration: identity.launch_configuration(),
            options_digest: identity.options_digest(),
            dimension: identity.dimension(),
            normalization: match identity.normalization() {
                backend_compile::EmbeddingNormalization::None => {
                    EmbeddingRefreshNormalization::None
                }
                backend_compile::EmbeddingNormalization::L2 => EmbeddingRefreshNormalization::L2,
            },
            maximum_text_bytes: identity.maximum_text_bytes(),
        }
    }
}

/// Compiler receipt that survives waiter loss and a later query refusal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EmbeddingRefreshCompilerReceipt {
    /// Old generation is retired; no replacement is advertised.
    Retired {
        /// Exact desired captured configuration.
        configuration: [u8; 32],
    },
    /// Actual compiler generation committed its real activated runtime.
    Activated {
        /// Exact committed configuration.
        configuration: [u8; 32],
        /// Actual activated execution identity.
        execution: EmbeddingRefreshExecution,
    },
}

/// Query recipe admission is distinct from building a complete semantic corpus.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EmbeddingRefreshQueryStatus {
    /// The query owner retains the compiler's exact activated Arc; corpus work remains lazy.
    Admitted,
    /// Compiler commit remains valid, but query installation refused.
    Unavailable {
        /// Actual query/control refusal.
        cause: EmbeddingRefreshFailure,
    },
}

/// Bounded owner observation. Accepted and committed states never imply corpus coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EmbeddingRefreshObservation {
    /// Still owned and pending under the original absolute operation deadline.
    Pending {
        /// Exact operation reference.
        ticket: EmbeddingRefreshTicket,
        /// Actual worker phase.
        phase: EmbeddingRefreshPhase,
    },
    /// Runtime committed; semantic source coverage still requires indexing under this recipe.
    Committed {
        /// Exact accepted operation reference.
        ticket: EmbeddingRefreshTicket,
        /// Actual compiler commit, never inferred from a successful save.
        execution: EmbeddingRefreshExecution,
        /// Query admission outcome, without advertising corpus availability.
        query: EmbeddingRefreshQueryStatus,
    },
    /// Accepted operation stopped; an earlier irreversible receipt is retained.
    Failed {
        /// Exact accepted operation reference.
        ticket: EmbeddingRefreshTicket,
        /// Actual failure.
        cause: EmbeddingRefreshFailure,
        /// Last actual compiler receipt, if that boundary was reached.
        compiler: Option<EmbeddingRefreshCompilerReceipt>,
    },
    /// Refused before an operation was accepted; no old owner was moved.
    Refused {
        /// Actual admission refusal.
        cause: EmbeddingRefreshFailure,
    },
    /// This owner has no retained operation matching the full ticket.
    Unknown {
        /// Requested exact ticket, preserved for caller recovery.
        ticket: EmbeddingRefreshTicket,
    },
}
impl EmbeddingRefreshObservation {
    /// Returns the exact accepted or requested observation reference; a pre-admission
    /// refusal has no ticket and cannot authorize observation or cancellation.
    #[must_use]
    pub const fn ticket(&self) -> Option<&EmbeddingRefreshTicket> {
        match self {
            Self::Pending { ticket, .. }
            | Self::Committed { ticket, .. }
            | Self::Failed { ticket, .. }
            | Self::Unknown { ticket } => Some(ticket),
            Self::Refused { .. } => None,
        }
    }

    /// Admits a reply against the original typed action, not merely its command family.
    /// # Errors
    /// Refuses a different configuration, any changed ticket field, or an impossible family.
    pub(crate) fn admit_for_action(
        &self,
        action: &EmbeddingRefreshAction,
    ) -> Result<(), ProductAdmissionError> {
        self.admit()?;
        let matches = match action {
            EmbeddingRefreshAction::Start { configuration, .. } => match self {
                Self::Refused { .. } => true,
                Self::Unknown { .. } => false,
                _ => self
                    .ticket()
                    .is_some_and(|ticket| ticket.configuration == *configuration),
            },
            EmbeddingRefreshAction::Observe { ticket }
            | EmbeddingRefreshAction::Cancel { ticket } => self.ticket() == Some(ticket),
        };
        if matches {
            Ok(())
        } else {
            Err(ProductAdmissionError::EmbeddingRefreshShape)
        }
    }

    pub(super) fn admit(&self) -> Result<(), ProductAdmissionError> {
        let valid_execution = |value: &EmbeddingRefreshExecution| {
            let Some(dimension) = u16::try_from(value.dimension)
                .ok()
                .and_then(std::num::NonZeroU16::new)
            else {
                return false;
            };
            let Some(text) = std::num::NonZeroU32::new(value.maximum_text_bytes) else {
                return false;
            };
            let spec = backend_compile::EmbeddingRuntimeSpecV1::new(
                value.model,
                value.model_version,
                value.tokenizer,
                value.executable,
                dimension,
                match value.normalization {
                    EmbeddingRefreshNormalization::None => {
                        backend_compile::EmbeddingNormalization::None
                    }
                    EmbeddingRefreshNormalization::L2 => {
                        backend_compile::EmbeddingNormalization::L2
                    }
                },
                text,
                value.options_digest,
            );
            spec.recipe_identity() == value.recipe
        };
        match self {
            Self::Committed { execution, .. } if !valid_execution(execution) => {
                Err(ProductAdmissionError::EmbeddingRefreshShape)
            }
            Self::Failed {
                ticket,
                compiler:
                    Some(EmbeddingRefreshCompilerReceipt::Activated {
                        configuration,
                        execution,
                    }),
                ..
            } if ticket.configuration != *configuration || !valid_execution(execution) => {
                Err(ProductAdmissionError::EmbeddingRefreshShape)
            }
            Self::Failed {
                ticket,
                compiler: Some(EmbeddingRefreshCompilerReceipt::Retired { configuration }),
                ..
            } if ticket.configuration != *configuration => {
                Err(ProductAdmissionError::EmbeddingRefreshShape)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandId, SurfaceCommand, SurfaceReply};

    #[test]
    fn refresh_wire_binds_every_ticket_field_and_refuses_old_live_peers_before_payloads() {
        use crate::{Command, CommandDto, CommandReply, ReplyDto, admit_reply, decode_reply_body};
        assert_eq!(crate::DTO_VERSION, 29);
        let ticket = EmbeddingRefreshTicket {
            sequence: NonZeroU64::MIN,
            owner_epoch: 19,
            owner_fence: [0x28; 32],
            configuration: [0x71; 32],
        };
        for request in [
            EmbeddingRefreshAction::Start {
                configuration: ticket.configuration,
                timeout_seconds: 30,
            },
            EmbeddingRefreshAction::Observe {
                ticket: ticket.clone(),
            },
            EmbeddingRefreshAction::Cancel {
                ticket: ticket.clone(),
            },
        ] {
            let command = CommandDto::new(
                51,
                Command::Surface(SurfaceCommand::EmbeddingRefresh {
                    request: request.clone(),
                }),
            );
            let observation = EmbeddingRefreshObservation::Pending {
                ticket: ticket.clone(),
                phase: EmbeddingRefreshPhase::Activating,
            };
            let reply = ReplyDto::new(
                51,
                CommandReply::Surface(SurfaceReply::EmbeddingRefresh(observation.clone())),
            );
            let bytes = serde_json::to_vec(&reply).expect("current exact reply");
            let decoded = decode_reply_body(&bytes).expect("current wire");
            admit_reply(&command, &decoded).expect("exact action identity");
            for field in 0..4 {
                let mut changed = ticket.clone();
                match field {
                    0 => changed.configuration[31] ^= 1,
                    1 => changed.owner_fence[31] ^= 1,
                    2 => changed.owner_epoch += 1,
                    _ => changed.sequence = NonZeroU64::new(2).expect("different sequence"),
                }
                let foreign = EmbeddingRefreshObservation::Pending {
                    ticket: changed,
                    phase: EmbeddingRefreshPhase::Activating,
                };
                let foreign = ReplyDto::new(
                    51,
                    CommandReply::Surface(SurfaceReply::EmbeddingRefresh(foreign)),
                );
                // Start has no issued ticket yet, so it binds its exact config; observations
                // and explicit cancellation must match all four original reference fields.
                if field == 0 || !matches!(request, EmbeddingRefreshAction::Start { .. }) {
                    assert!(admit_reply(&command, &foreign).is_err(), "field {field}");
                }
            }
            let mut old: serde_json::Value =
                serde_json::from_slice(&bytes).expect("current envelope");
            old["version"] = serde_json::json!(28);
            old["reply"] = serde_json::json!({"unknown_future_variant": true});
            let error = decode_reply_body(&serde_json::to_vec(&old).expect("old header"))
                .expect_err("strict DTO28 live peer refused");
            assert!(error.contains("reply DTO version 28"));
            assert!(error.contains("this build supports 29"));
            assert!(!error.contains("unknown variant"));
        }
        let refused = EmbeddingRefreshObservation::Refused {
            cause: EmbeddingRefreshFailure {
                kind: EmbeddingRefreshFailureKind::Busy,
                detail: ProductText::from_static("actual writer busy before admission"),
            },
        };
        assert!(
            refused
                .admit_for_action(&EmbeddingRefreshAction::Start {
                    configuration: ticket.configuration,
                    timeout_seconds: 30
                })
                .is_ok()
        );
        assert!(
            refused
                .admit_for_action(&EmbeddingRefreshAction::Observe {
                    ticket: ticket.clone()
                })
                .is_err()
        );
        assert!(
            EmbeddingRefreshObservation::Unknown {
                ticket: ticket.clone()
            }
            .admit_for_action(&EmbeddingRefreshAction::Start {
                configuration: ticket.configuration,
                timeout_seconds: 30
            })
            .is_err()
        );
    }

    #[test]
    fn refresh_contract_preserves_exact_ticket_and_refuses_inconsistent_commits() {
        let ticket = EmbeddingRefreshTicket {
            sequence: NonZeroU64::MIN,
            owner_epoch: 19,
            owner_fence: [0x28; 32],
            configuration: [0x71; 32],
        };
        for request in [
            EmbeddingRefreshAction::Start {
                configuration: ticket.configuration,
                timeout_seconds: 30,
            },
            EmbeddingRefreshAction::Observe {
                ticket: ticket.clone(),
            },
            EmbeddingRefreshAction::Cancel {
                ticket: ticket.clone(),
            },
        ] {
            let command = SurfaceCommand::EmbeddingRefresh { request };
            command.admit().expect("bounded original control");
            assert_eq!(
                command.mutation(),
                match &command {
                    SurfaceCommand::EmbeddingRefresh {
                        request: EmbeddingRefreshAction::Observe { .. },
                    } => crate::CommandMutation::Read,
                    _ => crate::CommandMutation::Write,
                }
            );
            let decoded: SurfaceCommand =
                serde_json::from_slice(&serde_json::to_vec(&command).expect("encode"))
                    .expect("decode");
            assert_eq!(decoded, command);
            assert_eq!(decoded.id(), CommandId::EmbeddingRefresh);
        }
        for timeout_seconds in [0, 3601, u16::MAX] {
            assert_eq!(
                EmbeddingRefreshAction::Start {
                    configuration: ticket.configuration,
                    timeout_seconds
                }
                .admit(),
                Err(ProductAdmissionError::EmbeddingRefreshShape)
            );
        }
        let spec = backend_compile::EmbeddingRuntimeSpecV1::new(
            [1; 32],
            [2; 32],
            [3; 32],
            [4; 32],
            std::num::NonZeroU16::new(3).expect("dimension"),
            backend_compile::EmbeddingNormalization::L2,
            std::num::NonZeroU32::new(4096).expect("text"),
            [5; 32],
        );
        let execution = EmbeddingRefreshExecution {
            model: spec.model(),
            model_version: spec.model_version(),
            tokenizer: spec.tokenizer(),
            executable: spec.executable(),
            recipe: spec.recipe_identity(),
            launch_configuration: [6; 32],
            options_digest: spec.options_digest(),
            dimension: 3,
            normalization: EmbeddingRefreshNormalization::L2,
            maximum_text_bytes: 4096,
        };
        let observation = EmbeddingRefreshObservation::Committed {
            ticket: ticket.clone(),
            execution: execution.clone(),
            query: EmbeddingRefreshQueryStatus::Unavailable {
                cause: EmbeddingRefreshFailure {
                    kind: EmbeddingRefreshFailureKind::Deadline,
                    detail: ProductText::from_static(
                        "original query admission deadline expired after compiler commit",
                    ),
                },
            },
        };
        let reply = SurfaceReply::EmbeddingRefresh(observation);
        reply
            .admit(CommandId::EmbeddingRefresh)
            .expect("actual commit retained despite query refusal");
        assert_eq!(
            serde_json::from_slice::<SurfaceReply>(&serde_json::to_vec(&reply).expect("encode"))
                .expect("decode"),
            reply
        );
        for mutation in 0..4 {
            let mut changed = execution.clone();
            match mutation {
                0 => changed.model_version[31] ^= 1,
                1 => changed.options_digest[31] ^= 1,
                2 => changed.dimension = 0,
                _ => changed.maximum_text_bytes = 0,
            }
            let bad = EmbeddingRefreshObservation::Committed {
                ticket: ticket.clone(),
                execution: changed,
                query: EmbeddingRefreshQueryStatus::Admitted,
            };
            assert_eq!(
                bad.admit(),
                Err(ProductAdmissionError::EmbeddingRefreshShape)
            );
        }
        let mut foreign = ticket.configuration;
        foreign[31] ^= 1;
        let bad = EmbeddingRefreshObservation::Failed {
            ticket,
            cause: EmbeddingRefreshFailure {
                kind: EmbeddingRefreshFailureKind::Activation,
                detail: ProductText::from_static("real helper refusal"),
            },
            compiler: Some(EmbeddingRefreshCompilerReceipt::Retired {
                configuration: foreign,
            }),
        };
        assert_eq!(
            bad.admit(),
            Err(ProductAdmissionError::EmbeddingRefreshShape),
            "same first 31 bytes cannot alias the accepted configuration"
        );
        assert!(serde_json::from_str::<EmbeddingRefreshAction>(r#"{"action":"observe","ticket":{"sequence":0,"owner_epoch":19,"owner_fence":[],"configuration":[]}}"#).is_err());
    }
}
