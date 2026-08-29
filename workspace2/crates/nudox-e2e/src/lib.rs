#![forbid(unsafe_code)]
//! Cross-crate Wave 1 public-API proof reusable by server-only observation adapters.

mod scenario;

pub use scenario::{
    InsertStep, OperationStep, PollClass, ScenarioError, ScenarioEvidence, ScenarioProbe,
    ScratchStep, run_wave1_scenario,
};
