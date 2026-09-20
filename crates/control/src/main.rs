//! Small JSON command adapter for `.config` and shell orchestration.

mod cli;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("backend-control: {error}");
        std::process::exit(1);
    }
}
