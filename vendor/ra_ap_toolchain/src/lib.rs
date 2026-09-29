//! Discovery of `cargo` & `rustc` executables.

use std::{
    env,
    ffi::OsStr,
    iter,
    path::{Path, PathBuf},
    process::Command,
};

use camino::{Utf8Path, Utf8PathBuf};

/// Internal environment-map entry used by rust-analyzer callers that need child
/// processes to inherit only the variables explicitly present in `extra_env`.
/// The marker itself is never forwarded to the child process.
pub const ISOLATE_ENV_MARKER: &str = "__RA_AP_TOOLCHAIN_ISOLATE_ENV";

#[derive(Copy, Clone)]
pub enum Tool {
    Cargo,
    Rustc,
    Rustup,
    Rustfmt,
}

impl Tool {
    pub fn proxy(self) -> Option<Utf8PathBuf> {
        cargo_proxy(self.name())
    }

    /// Return a `PathBuf` to use for the given executable.
    ///
    /// The current implementation checks three places for an executable to use:
    /// 1) `$CARGO_HOME/bin/<executable_name>`
    ///    where $CARGO_HOME defaults to ~/.cargo (see <https://doc.rust-lang.org/cargo/guide/cargo-home.html>)
    ///    example: for cargo, this tries $CARGO_HOME/bin/cargo, or ~/.cargo/bin/cargo if $CARGO_HOME is unset.
    ///    It seems that this is a reasonable place to try for cargo, rustc, and rustup
    /// 2) Appropriate environment variable (erroring if this is set but not a usable executable)
    ///    example: for cargo, this checks $CARGO environment variable; for rustc, $RUSTC; etc
    /// 3) $PATH/`<executable_name>`
    ///    example: for cargo, this tries all paths in $PATH with appended `cargo`, returning the
    ///    first that exists
    /// 4) If all else fails, we just try to use the executable name directly
    pub fn prefer_proxy(self) -> Utf8PathBuf {
        invoke(&[cargo_proxy, lookup_as_env_var, lookup_in_path], self.name())
    }

    /// Resolve this executable from an explicit environment overlay, falling
    /// back to the process environment only when that overlay does not isolate
    /// child processes.
    pub fn prefer_proxy_with_env<H>(
        self,
        extra_env: &std::collections::HashMap<String, Option<String>, H>,
    ) -> Utf8PathBuf
    where
        H: std::hash::BuildHasher,
    {
        lookup_as_env_var_with_env(self.name(), extra_env)
            .or_else(|| cargo_proxy_with_env(self.name(), extra_env))
            .or_else(|| lookup_in_path_with_env(self.name(), extra_env))
            .unwrap_or_else(|| self.name().into())
    }

    /// Return a `PathBuf` to use for the given executable.
    ///
    /// The current implementation checks three places for an executable to use:
    /// 1) Appropriate environment variable (erroring if this is set but not a usable executable)
    ///    example: for cargo, this checks $CARGO environment variable; for rustc, $RUSTC; etc
    /// 2) $PATH/`<executable_name>`
    ///    example: for cargo, this tries all paths in $PATH with appended `cargo`, returning the
    ///    first that exists
    /// 3) `$CARGO_HOME/bin/<executable_name>`
    ///    where $CARGO_HOME defaults to ~/.cargo (see <https://doc.rust-lang.org/cargo/guide/cargo-home.html>)
    ///    example: for cargo, this tries $CARGO_HOME/bin/cargo, or ~/.cargo/bin/cargo if $CARGO_HOME is unset.
    ///    It seems that this is a reasonable place to try for cargo, rustc, and rustup
    /// 4) If all else fails, we just try to use the executable name directly
    pub fn path(self) -> Utf8PathBuf {
        invoke(&[lookup_as_env_var, lookup_in_path, cargo_proxy], self.name())
    }

    /// Resolve this executable from an explicit environment overlay, falling
    /// back to the process environment only when that overlay does not isolate
    /// child processes.
    pub fn path_with_env<H>(
        self,
        extra_env: &std::collections::HashMap<String, Option<String>, H>,
    ) -> Utf8PathBuf
    where
        H: std::hash::BuildHasher,
    {
        lookup_as_env_var_with_env(self.name(), extra_env)
            .or_else(|| lookup_in_path_with_env(self.name(), extra_env))
            .or_else(|| cargo_proxy_with_env(self.name(), extra_env))
            .unwrap_or_else(|| self.name().into())
    }

    pub fn path_in(self, path: &Utf8Path) -> Option<Utf8PathBuf> {
        probe_for_binary(path.join(self.name()))
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Cargo => "cargo",
            Tool::Rustc => "rustc",
            Tool::Rustup => "rustup",
            Tool::Rustfmt => "rustfmt",
        }
    }
}

// Prevent rustup from automatically installing toolchains, see https://github.com/rust-lang/rust-analyzer/issues/20719.
pub const NO_RUSTUP_AUTO_INSTALL_ENV: (&str, &str) = ("RUSTUP_AUTO_INSTALL", "0");

#[allow(clippy::disallowed_types)] /* generic parameter allows for FxHashMap */
pub fn command<H>(
    cmd: impl AsRef<OsStr>,
    working_directory: impl AsRef<Path>,
    extra_env: &std::collections::HashMap<String, Option<String>, H>,
) -> Command
where
    H: std::hash::BuildHasher,
{
    // we are `toolchain::command``
    #[allow(clippy::disallowed_methods)]
    let mut cmd = Command::new(cmd);
    if isolates_env(extra_env) {
        cmd.env_clear();
    }
    cmd.current_dir(working_directory);
    cmd.env(NO_RUSTUP_AUTO_INSTALL_ENV.0, NO_RUSTUP_AUTO_INSTALL_ENV.1);
    for env in extra_env {
        if env.0 == ISOLATE_ENV_MARKER {
            continue;
        }
        match env {
            (key, Some(val)) => cmd.env(key, val),
            (key, None) => cmd.env_remove(key),
        };
    }
    cmd
}

/// Returns whether this overlay requests child-process environment isolation.
pub fn isolates_env<H>(extra_env: &std::collections::HashMap<String, Option<String>, H>) -> bool
where
    H: std::hash::BuildHasher,
{
    extra_env
        .get(ISOLATE_ENV_MARKER)
        .is_some_and(|value| value.as_deref() == Some("1"))
}

fn invoke(list: &[fn(&str) -> Option<Utf8PathBuf>], executable: &str) -> Utf8PathBuf {
    list.iter().find_map(|it| it(executable)).unwrap_or_else(|| executable.into())
}

/// Looks up the binary as its SCREAMING upper case in the env variables.
fn lookup_as_env_var(executable_name: &str) -> Option<Utf8PathBuf> {
    env::var_os(executable_name.to_ascii_uppercase())
        .map(PathBuf::from)
        .map(Utf8PathBuf::try_from)
        .and_then(Result::ok)
}

fn lookup_as_env_var_with_env<H>(
    executable_name: &str,
    extra_env: &std::collections::HashMap<String, Option<String>, H>,
) -> Option<Utf8PathBuf>
where
    H: std::hash::BuildHasher,
{
    let key = executable_name.to_ascii_uppercase();
    match extra_env.get(&key) {
        Some(Some(path)) => Some(Utf8PathBuf::from(path)),
        Some(None) => None,
        None if isolates_env(extra_env) => None,
        None => lookup_as_env_var(executable_name),
    }
}

/// Looks up the binary in the cargo home directory if it exists.
fn cargo_proxy(executable_name: &str) -> Option<Utf8PathBuf> {
    let mut path = get_cargo_home()?;
    path.push("bin");
    path.push(executable_name);
    probe_for_binary(path)
}

fn cargo_proxy_with_env<H>(
    executable_name: &str,
    extra_env: &std::collections::HashMap<String, Option<String>, H>,
) -> Option<Utf8PathBuf>
where
    H: std::hash::BuildHasher,
{
    let mut path = get_cargo_home_with_env(extra_env)?;
    path.push("bin");
    path.push(executable_name);
    probe_for_binary(path)
}

fn get_cargo_home() -> Option<Utf8PathBuf> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return Utf8PathBuf::try_from(PathBuf::from(path)).ok();
    }

    if let Some(mut path) = env::home_dir() {
        path.push(".cargo");
        return Utf8PathBuf::try_from(path).ok();
    }

    None
}

fn get_cargo_home_with_env<H>(
    extra_env: &std::collections::HashMap<String, Option<String>, H>,
) -> Option<Utf8PathBuf>
where
    H: std::hash::BuildHasher,
{
    match extra_env.get("CARGO_HOME") {
        Some(Some(path)) => return Utf8PathBuf::try_from(PathBuf::from(path)).ok(),
        Some(None) => return None,
        None if isolates_env(extra_env) => return None,
        None => {}
    }
    get_cargo_home()
}

fn lookup_in_path(exec: &str) -> Option<Utf8PathBuf> {
    let paths = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&paths)
        .map(|path| path.join(exec))
        .map(Utf8PathBuf::try_from)
        .filter_map(Result::ok)
        .find_map(probe_for_binary)
}

fn lookup_in_path_with_env<H>(
    exec: &str,
    extra_env: &std::collections::HashMap<String, Option<String>, H>,
) -> Option<Utf8PathBuf>
where
    H: std::hash::BuildHasher,
{
    let paths = match extra_env.get("PATH") {
        Some(Some(paths)) => paths.clone().into(),
        Some(None) => return None,
        None if isolates_env(extra_env) => return None,
        None => env::var_os("PATH").unwrap_or_default(),
    };
    env::split_paths(&paths)
        .map(|path| path.join(exec))
        .map(Utf8PathBuf::try_from)
        .filter_map(Result::ok)
        .find_map(probe_for_binary)
}

pub fn probe_for_binary(path: Utf8PathBuf) -> Option<Utf8PathBuf> {
    let with_extension = match env::consts::EXE_EXTENSION {
        "" => None,
        it => Some(path.with_extension(it)),
    };
    iter::once(path).chain(with_extension).find(|it| it.is_file())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::{ISOLATE_ENV_MARKER, Tool};

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ra-ap-toolchain-env-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn executable(path: &PathBuf) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture").unwrap();
    }

    #[test]
    fn explicit_tool_variables_override_process_discovery() {
        let dir = temp_dir();
        let cargo = dir.join("selected-cargo");
        let rustc = dir.join("selected-rustc");
        executable(&cargo);
        executable(&rustc);

        let extra_env = HashMap::from([
            ("CARGO".to_owned(), Some(cargo.to_string_lossy().into_owned())),
            ("RUSTC".to_owned(), Some(rustc.to_string_lossy().into_owned())),
            (ISOLATE_ENV_MARKER.to_owned(), Some("1".to_owned())),
        ]);

        assert_eq!(Tool::Cargo.path_with_env(&extra_env).as_str(), cargo.to_str().unwrap());
        assert_eq!(Tool::Rustc.path_with_env(&extra_env).as_str(), rustc.to_str().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn isolated_lookup_uses_the_supplied_path_and_ignores_process_path() {
        let dir = temp_dir();
        let cargo = dir.join("bin/cargo");
        executable(&cargo);
        let extra_env = HashMap::from([
            ("PATH".to_owned(), Some(dir.join("bin").to_string_lossy().into_owned())),
            (ISOLATE_ENV_MARKER.to_owned(), Some("1".to_owned())),
        ]);

        assert_eq!(Tool::Cargo.path_with_env(&extra_env).as_str(), cargo.to_str().unwrap());

        let isolated_without_path =
            HashMap::from([(ISOLATE_ENV_MARKER.to_owned(), Some("1".to_owned()))]);
        assert_eq!(Tool::Cargo.path_with_env(&isolated_without_path).as_str(), "cargo");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn isolated_cargo_home_selects_its_proxy() {
        let dir = temp_dir();
        let cargo = dir.join("bin/cargo");
        executable(&cargo);
        let extra_env = HashMap::from([
            ("CARGO_HOME".to_owned(), Some(dir.to_string_lossy().into_owned())),
            (ISOLATE_ENV_MARKER.to_owned(), Some("1".to_owned())),
        ]);

        assert_eq!(Tool::Cargo.prefer_proxy_with_env(&extra_env).as_str(), cargo.to_str().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
