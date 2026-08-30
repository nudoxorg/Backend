#![allow(dead_code)]

struct BorrowedScenario {
    step: &'static str,
}

struct OwnedScenario {
    expected: String,
}

macro_rules! local_state {
    ($name:ident) => {
        struct $name {
            observed: &'static str,
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

struct DiagnosticMessage {
    message: &'static str,
}

fn main() {}
