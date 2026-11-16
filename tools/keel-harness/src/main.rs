//! Artifact-driven integration runner; unsupported platforms refuse explicitly.
#[cfg(unix)]
mod assertion;
#[cfg(unix)]
mod driver;
#[cfg(unix)]
mod isolation;
#[cfg(unix)]
mod manifest;
#[cfg(unix)]
mod process;

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        if let Some(code) = isolation::exec_helper() {
            return code;
        }
        driver::entry()
    }
    #[cfg(not(unix))]
    {
        eprintln!(
            "keel: this integration runner requires Unix process groups and local sockets; no tests were run"
        );
        std::process::ExitCode::from(2)
    }
}
