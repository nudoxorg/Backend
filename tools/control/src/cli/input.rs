//! Input admission and durable-ledger setup for the command adapter.

use std::env;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use backend_control::ids::{AgentWorkKey, Identity, OwnerSchema, decode_hex};
use backend_control::{ControlError, DurableControlPlane, SchedulerLimits};

pub(crate) fn open(arguments: &[String]) -> Result<DurableControlPlane, ControlError> {
    let path = optional(arguments, "--ledger")
        .map(PathBuf::from)
        .or_else(|| env::var_os("BACKEND_CONTROL_LEDGER").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(".config/state/agent-control"));
    let max_pack_bytes = optional(arguments, "--max-pack-bytes")
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| ControlError::Wire("invalid pack size".to_owned()))?
        .unwrap_or(4 * 1024 * 1024);
    DurableControlPlane::open(path, max_pack_bytes, SchedulerLimits::default())
}

pub(crate) fn request_json(arguments: &[String]) -> Result<String, ControlError> {
    if let Some(value) = optional(arguments, "--json") {
        return Ok(value.to_owned());
    }
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| ControlError::Wire(error.to_string()))?;
    if input.trim().is_empty() {
        return Err(ControlError::MissingArgument("--json or stdin"));
    }
    Ok(input)
}

pub(crate) fn required<'a>(
    arguments: &'a [String],
    name: &'static str,
) -> Result<&'a str, ControlError> {
    optional(arguments, name).ok_or(ControlError::MissingArgument(name))
}

pub(crate) fn optional<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|window| window[0] == name)
        .map(|window| window[1].as_str())
}

pub(crate) fn admit_key(
    plane: &DurableControlPlane,
    value: &str,
) -> Result<AgentWorkKey, ControlError> {
    plane.lookup_work_key(&decode_hex(value)?)
}

pub(crate) fn fence(arguments: &[String]) -> Result<Vec<u8>, ControlError> {
    decode_hex(required(arguments, "--fence")?)
}

pub(crate) fn owner(value: &str) -> Result<Identity<OwnerSchema>, ControlError> {
    Identity::<OwnerSchema>::from_label(value)
}

pub(crate) fn now() -> Result<u64, ControlError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ControlError::Wire("system clock precedes epoch".to_owned()))
        .and_then(|duration| u64::try_from(duration.as_nanos()).map_err(|_| ControlError::Bounds))
}
