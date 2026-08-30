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

fn exact_from_bytes(_bytes: &[u8]) -> ConfigError {
    ConfigError::Allocation
}

fn cold_raw_rescan(result: Result<u8, ParseError>, bytes: &[u8]) -> Result<u8, ConfigError> {
    result.map_err(|_| exact_from_bytes(bytes))
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

    pub const fn bytes_of(fact: &Self) -> usize {
        fact.bytes
    }
}

trait PublicFactView {
    fn bytes(&self) -> usize;
}

impl PublicFactView for PublicFact {
    fn bytes(&self) -> usize {
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

trait Source {}

fn dynamically_dispatched(source: &dyn Source) {
    let _ = source;
}

#[allow(
    nudox_dynamic_dispatch,
    reason = "the process-local plugin adapter is an intentionally erased cold boundary"
)]
fn earned_plugin_boundary(source: &dyn Source) {
    let _ = source;
}

fn statically_dispatched(source: &impl Source) {
    let _ = source;
}

fn main() {}
