//! Run a real Rust test in a fresh process without leaking process-wide hooks.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_CASE: &str = "NUDOX_TEST_ISOLATED_CASE";
const CHILD_WITNESS: &str = "NUDOX_TEST_ISOLATED_WITNESS";
const CHILD_DEADLINE: Duration = Duration::from_secs(3600);

/// Returns true in the parent only after the exact child test entered and
/// passed. Returns false in that child so the original test body runs normally.
///
/// Both the successful child status and its private entry witness are required;
/// libtest's successful zero-test result cannot acknowledge this test. The
/// child runs one test on one harness thread, so process-wide frontend hooks
/// cannot affect another test in the parent harness.
pub(crate) fn run_test_in_child(module: &str, case: &str) -> io::Result<bool> {
    run_test_in_child_with_env(module, case, &[])
}

/// Gives the isolated test its explicit environment before process startup.
/// Parent-harness environment and cached process configuration stay unchanged.
pub(crate) fn run_test_in_child_with_env(
    module: &str,
    case: &str,
    environment: &[(&str, &str)],
) -> io::Result<bool> {
    if environment
        .iter()
        .any(|(key, _)| *key == CHILD_CASE || *key == CHILD_WITNESS)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "isolated test environment cannot replace its execution protocol",
        ));
    }
    let test = full_test_name(module, case)?;
    if std::env::var(CHILD_CASE).as_deref() == Ok(test.as_str()) {
        if !std::env::args_os().any(|arg| arg == "--exact")
            || !std::env::args_os().any(|arg| arg == test.as_str())
        {
            return Err(io::Error::other(
                "isolated test child lacks its exact harness selection",
            ));
        }
        for (key, value) in environment {
            if std::env::var_os(key).as_deref() != Some(std::ffi::OsStr::new(value)) {
                return Err(io::Error::other(format!(
                    "isolated child environment differs for {key}"
                )));
            }
        }
        let witness = std::env::var_os(CHILD_WITNESS)
            .ok_or_else(|| io::Error::other("isolated test child lacks its entry witness"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(witness)?;
        file.write_all(test.as_bytes())?;
        return Ok(false);
    }

    let directory = super::private_temp_dir("native-test-child")?;
    let witness = directory.path().join("entered");
    let mut child = ReapedChild(
        Command::new(std::env::current_exe()?)
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(CHILD_CASE, &test)
            .env(CHILD_WITNESS, &witness)
            .envs(environment.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() >= CHILD_DEADLINE {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("isolated test exceeded its process deadline: {test}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !status.success() {
        return Err(io::Error::other(format!(
            "isolated test failed: {test}: {status}"
        )));
    }
    let entered = fs::read(&witness).map_err(|error| {
        io::Error::other(format!(
            "isolated child did not enter its selected test {test}: {error}"
        ))
    })?;
    if entered != test.as_bytes() {
        return Err(io::Error::other(
            "isolated child acknowledged a different test",
        ));
    }
    Ok(true)
}

fn full_test_name(module: &str, case: &str) -> io::Result<String> {
    let (_, module) = module
        .split_once("::")
        .ok_or_else(|| io::Error::other("isolated test module lacks its crate prefix"))?;
    Ok(format!("{module}::{case}"))
}

struct ReapedChild(Child);
impl Drop for ReapedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static PROCESS_FLAG: AtomicBool = AtomicBool::new(false);

    #[test]
    fn isolated_child_starts_with_fresh_process_state() -> io::Result<()> {
        let test = full_test_name(
            module_path!(),
            "isolated_child_starts_with_fresh_process_state",
        )?;
        if std::env::var(CHILD_CASE).as_deref() != Ok(test.as_str()) {
            PROCESS_FLAG.store(true, Ordering::Release);
        }
        if run_test_in_child(
            module_path!(),
            "isolated_child_starts_with_fresh_process_state",
        )? {
            assert!(
                PROCESS_FLAG.swap(false, Ordering::AcqRel),
                "child changed the parent's process state"
            );
            return Ok(());
        }
        assert!(
            !PROCESS_FLAG.load(Ordering::Acquire),
            "child inherited the parent's process-wide hook"
        );
        Ok(())
    }

    #[test]
    fn successful_zero_test_child_is_refused() {
        let result = run_test_in_child(module_path!(), "this_case_does_not_exist");
        assert!(
            result.is_err_and(|error| error.to_string().contains("did not enter")),
            "a successful empty libtest selection cannot acknowledge execution"
        );
    }

    #[test]
    fn child_environment_is_set_before_startup_without_changing_parent() -> io::Result<()> {
        const KEY: &str = "NUDOX_TEST_ISOLATED_CONTROL_VALUE";
        let parent_value = std::env::var_os(KEY);
        if run_test_in_child_with_env(
            module_path!(),
            "child_environment_is_set_before_startup_without_changing_parent",
            &[(KEY, "child-only")],
        )? {
            assert_eq!(std::env::var_os(KEY), parent_value);
            return Ok(());
        }
        assert_eq!(std::env::var(KEY).as_deref(), Ok("child-only"));
        Ok(())
    }
}
