#![allow(dead_code)]

#[derive(Debug)]
enum ConfigError {
    Allocation,
    Exact { source: ParseError },
}

#[derive(Clone, Copy, Debug)]
struct ParseError;

fn erased(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    result.map_err(|_| ConfigError::Allocation)
}

fn retained(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    result.map_err(|source| ConfigError::Exact { source })
}

fn exact_rescan(_source: ParseError) -> ConfigError {
    ConfigError::Allocation
}

fn cold_diagnostic_rescan(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    result.map_err(|source| exact_rescan(source))
}

struct Scenario {
    step: &'static str,
}

enum ScenarioStep {
    Start,
}

struct TypedScenario {
    step: ScenarioStep,
}

pub struct PublicFact {
    pub bytes: usize,
}

impl PublicFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

pub struct ProtectedFact {
    bytes: usize,
}

impl ProtectedFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

fn main() {}

