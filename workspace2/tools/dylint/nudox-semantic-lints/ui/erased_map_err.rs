#![allow(dead_code)]

#[derive(Debug)]
enum ConfigError {
    Allocation,
    Exact { source: ParseError },
}

#[derive(Clone, Copy, Debug)]
struct ParseError;

fn wildcard(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    result.map_err(|_| ConfigError::Allocation)
}

fn named_but_unused(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    result.map_err(|_source| ConfigError::Allocation)
}

macro_rules! erase_locally {
    ($result:expr) => {
        $result.map_err(|_| ConfigError::Allocation)
    };
}

fn expanded(result: Result<u8, ParseError>) -> Result<u8, ConfigError> {
    erase_locally!(result)
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

struct Unrelated;

impl Unrelated {
    fn map_err(self, replace: impl FnOnce(ParseError) -> ConfigError) -> ConfigError {
        replace(ParseError)
    }
}

fn unrelated_method(value: Unrelated) -> ConfigError {
    value.map_err(|_| ConfigError::Allocation)
}

fn main() {}
