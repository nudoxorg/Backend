//! Process CLI that only decodes positional fields and presents service replies.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use wave_application_core::{ApplicationOutcome, ApplicationService};
use wave_application_protocol::{
    CLI_COMMAND_SEPARATOR, collect_cli_arguments, decode_cli, encode_cli_adapter_error,
    encode_cli_reply,
};

fn main() -> ExitCode {
    let arguments = match collect_cli_arguments(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(error) => return transport_failure(&error),
    };
    let mut service = ApplicationService::new();
    let mut business_failure = false;
    for command in arguments.split(|argument| argument == CLI_COMMAND_SEPARATOR) {
        let input = match decode_cli(command) {
            Ok(input) => input,
            Err(error) => return transport_failure(&error),
        };
        let reply = service.execute(&input);
        let failed = matches!(&reply.outcome, ApplicationOutcome::Failed { .. });
        if write_json(encode_cli_reply(reply)).is_err() {
            return ExitCode::from(1);
        }
        business_failure |= failed;
    }
    if business_failure {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn transport_failure(error: &wave_application_protocol::AdapterError) -> ExitCode {
    if write_json(encode_cli_adapter_error(error)).is_err() {
        ExitCode::from(1)
    } else {
        ExitCode::from(64)
    }
}

fn write_json(encoded: io::Result<Vec<u8>>) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(&encoded?)?;
    output.write_all(b"\n")?;
    output.flush()
}
