//! Measures `backend-store` benches capacity-planning runner machine work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Bounded machine-provenance probes with explicit unavailable reasons.

use std::process::Command;

use crate::model::{MachineNumber, MachineProbeUnavailable, MachineText};

pub(crate) fn operating_system_release() -> MachineText {
    if cfg!(target_os = "macos") {
        machine_text("sw_vers", &["-productVersion"])
    } else {
        machine_text("uname", &["-r"])
    }
}

pub(crate) fn cpu_model() -> MachineText {
    if cfg!(target_os = "macos") {
        machine_text("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        MachineText::Unavailable {
            reason: MachineProbeUnavailable::UnsupportedPlatform,
        }
    }
}

pub(crate) fn physical_cpu_count() -> MachineNumber {
    if cfg!(target_os = "macos") {
        machine_number("sysctl", &["-n", "hw.physicalcpu"])
    } else {
        MachineNumber::Unavailable {
            reason: MachineProbeUnavailable::UnsupportedPlatform,
        }
    }
}

pub(crate) fn physical_memory_bytes() -> MachineNumber {
    if cfg!(target_os = "macos") {
        machine_number("sysctl", &["-n", "hw.memsize"])
    } else {
        MachineNumber::Unavailable {
            reason: MachineProbeUnavailable::UnsupportedPlatform,
        }
    }
}

fn machine_text(program: &str, arguments: &[&str]) -> MachineText {
    let Ok(output) = Command::new(program).args(arguments).output() else {
        return MachineText::Unavailable {
            reason: MachineProbeUnavailable::ProbeStartFailed,
        };
    };
    if !output.status.success() {
        return MachineText::Unavailable {
            reason: MachineProbeUnavailable::ProbeRejected,
        };
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return MachineText::Unavailable {
            reason: MachineProbeUnavailable::ProbeOutputInvalid,
        };
    };
    let value = text.trim();
    if value.is_empty() {
        MachineText::Unavailable {
            reason: MachineProbeUnavailable::ProbeOutputInvalid,
        }
    } else {
        MachineText::Measured {
            value: value.to_owned(),
        }
    }
}

fn machine_number(program: &str, arguments: &[&str]) -> MachineNumber {
    match machine_text(program, arguments) {
        MachineText::Measured { value } => match value.parse::<u64>() {
            Ok(value) => MachineNumber::Measured { value },
            Err(_) => MachineNumber::Unavailable {
                reason: MachineProbeUnavailable::ProbeValueInvalid,
            },
        },
        MachineText::Unavailable { reason } => MachineNumber::Unavailable { reason },
    }
}
