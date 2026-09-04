//! Runs the `interface-cli` executable, which exists to serve the unified application protocol over a command-line process.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! Process CLI that only decodes positional fields and presents service replies.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use interface_core::{ApplicationOutcome, ApplicationService};
use interface_protocol::{
    AdapterError, CLI_COMMAND_SEPARATOR, collect_cli_arguments, decode_cli_command,
    encode_cli_adapter_error, encode_cli_reply, write_human,
};

mod source;

use source::application_input;

#[derive(Clone, Copy)]
enum OutputMode {
    Json,
    Human,
}

fn main() -> ExitCode {
    let (mode, raw_arguments) = split_mode(std::env::args().skip(1));
    let arguments = match collect_cli_arguments(raw_arguments) {
        Ok(arguments) => arguments,
        Err(error) => return transport_failure(&error, mode),
    };
    let mut service = ApplicationService::new();
    let mut business_failure = false;
    let mut standard_input_consumed = false;
    for command in arguments.split(|argument| argument == CLI_COMMAND_SEPARATOR) {
        let command = match decode_cli_command(command) {
            Ok(command) => command,
            Err(error) => return transport_failure(&error, mode),
        };
        let input = match application_input(command, &mut standard_input_consumed) {
            Ok(input) => input,
            Err(error) => return transport_failure(&error, mode),
        };
        let reply = service.execute(&input);
        let failed = matches!(&reply.outcome, ApplicationOutcome::Failed { .. });
        let written = match mode {
            OutputMode::Json => write_json(encode_cli_reply(reply)),
            OutputMode::Human => write_human_reply(&reply),
        };
        if written.is_err() {
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

fn split_mode(
    arguments: impl Iterator<Item = String>,
) -> (OutputMode, impl Iterator<Item = String>) {
    let mut mode = OutputMode::Json;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.peek() {
        match argument.as_str() {
            "--human" => {
                mode = OutputMode::Human;
                let _ = arguments.next();
            }
            "--json" => {
                mode = OutputMode::Json;
                let _ = arguments.next();
            }
            _ => break,
        }
    }
    (mode, arguments)
}

fn transport_failure(error: &AdapterError, mode: OutputMode) -> ExitCode {
    let written = match mode {
        OutputMode::Json => write_json(encode_cli_adapter_error(error)),
        OutputMode::Human => write_human_transport_error(error),
    };
    if written.is_err() {
        ExitCode::from(1)
    } else {
        ExitCode::from(64)
    }
}

fn write_human_reply(reply: &interface_core::ApplicationReply) -> io::Result<()> {
    let mut rendered = String::new();
    write_human(reply, &mut rendered).map_err(io::Error::other)?;
    write_text(&rendered)
}

fn write_human_transport_error(error: &AdapterError) -> io::Result<()> {
    let mut rendered = String::from("error: transport\n  ");
    rendered.push_str(&error.to_string());
    rendered.push('\n');
    write_text(&rendered)
}

fn write_json(encoded: io::Result<Vec<u8>>) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(&encoded?)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn write_text(text: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(text.as_bytes())?;
    output.flush()
}
