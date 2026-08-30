#![allow(dead_code)]

struct BorrowedScenario {
    step: &'static str,
}

struct OwnedScenario {
    expected: String,
}

enum DecodeError {
    UnknownField { field: &'static str },
    InvalidDetail { detail: &'static str },
    WrongStage { stage: &'static str },
}

macro_rules! local_state {
    ($name:ident) => {
        struct $name {
            phase: &'static str,
        }
    };
}

local_state!(ExpandedScenario);

enum ScenarioStep {
    Start,
}

struct TypedScenario {
    step: ScenarioStep,
}

struct OpenResponse {
    observed: String,
}

struct BorrowedResponse<'response> {
    phase: &'response str,
}

struct DiagnosticMessage {
    message: &'static str,
}

fn main() {}
