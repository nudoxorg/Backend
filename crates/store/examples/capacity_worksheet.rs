//! Demonstrates `backend-store` examples capacity-worksheet composition through public APIs.
//! The example keeps all authority and resource inputs visible to its caller.
//! It doubles as executable documentation for the smallest complete journey.
#![deny(unsafe_code)]
//! Workload-driven capacity worksheet example; it is not a shipping executable.

use std::{
    env,
    fs::{self, File},
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;

#[path = "capacity_worksheet/capacity.rs"]
mod capacity;

use capacity::{CapacityInput, CapacityModelError, plan};

/// Capacity worksheet command failure with a closed argument or I/O cause.
#[derive(Debug, Error)]
enum WorksheetError {
    #[error("unrecognized argument {argument}")]
    UnknownArgument { argument: String },
    #[error("argument {argument} requires a value")]
    MissingValue { argument: WorksheetArgument },
    #[error("missing required --input JSON worksheet")]
    MissingInput,
    #[error("could not open worksheet {path}")]
    OpenInput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse worksheet JSON")]
    ParseInput(#[source] serde_json::Error),
    #[error("worksheet capacity model rejected its facts")]
    Capacity(#[source] CapacityModelError),
    #[error("could not create worksheet output parent {path}")]
    CreateParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not create worksheet output {path}")]
    CreateOutput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize worksheet result")]
    Serialize(#[source] serde_json::Error),
    #[error("could not flush worksheet result")]
    Flush(#[source] io::Error),
}

/// Closed command argument identity used by typed parser errors.
#[derive(Clone, Copy, Debug, Error)]
enum WorksheetArgument {
    #[error("--input")]
    Input,
    #[error("--output")]
    Output,
}

enum Command {
    Help,
    Plan {
        input: PathBuf,
        output: Option<PathBuf>,
    },
}

fn main() -> Result<(), WorksheetError> {
    match parse_command()? {
        Command::Help => {
            println!(
                "Usage: capacity-worksheet --input FACTS.json [--output PLAN.json]\n\nAll capacities are supplied facts. Omitting --output prints the plan to stdout."
            );
            Ok(())
        }
        Command::Plan { input, output } => write_plan(&read_plan(&input)?, output),
    }
}

fn parse_command() -> Result<Command, WorksheetError> {
    let mut input = None;
    let mut output = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--input" => {
                input = Some(PathBuf::from(next_value(
                    &mut arguments,
                    WorksheetArgument::Input,
                )?));
            }
            "--output" => {
                output = Some(PathBuf::from(next_value(
                    &mut arguments,
                    WorksheetArgument::Output,
                )?));
            }
            "--help" => return Ok(Command::Help),
            _ => return Err(WorksheetError::UnknownArgument { argument }),
        }
    }
    input
        .map(|input| Command::Plan { input, output })
        .ok_or(WorksheetError::MissingInput)
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    argument: WorksheetArgument,
) -> Result<String, WorksheetError> {
    arguments
        .next()
        .ok_or(WorksheetError::MissingValue { argument })
}

fn read_plan(path: &Path) -> Result<capacity::CapacityPlan, WorksheetError> {
    let file = File::open(path).map_err(|source| WorksheetError::OpenInput {
        path: path.to_path_buf(),
        source,
    })?;
    let input: CapacityInput =
        serde_json::from_reader(BufReader::new(file)).map_err(WorksheetError::ParseInput)?;
    plan(&input).map_err(WorksheetError::Capacity)
}

fn write_plan(
    plan: &capacity::CapacityPlan,
    output: Option<PathBuf>,
) -> Result<(), WorksheetError> {
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| WorksheetError::CreateParent {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = File::create(&path).map_err(|source| WorksheetError::CreateOutput {
            path: path.clone(),
            source,
        })?;
        serde_json::to_writer_pretty(&mut file, plan).map_err(WorksheetError::Serialize)?;
        file.write_all(b"\n").map_err(WorksheetError::Flush)?;
        file.flush().map_err(WorksheetError::Flush)
    } else {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        serde_json::to_writer_pretty(&mut output, plan).map_err(WorksheetError::Serialize)?;
        output.write_all(b"\n").map_err(WorksheetError::Flush)?;
        output.flush().map_err(WorksheetError::Flush)
    }
}
