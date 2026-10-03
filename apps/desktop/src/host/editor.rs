//! Opening a place in the editor: `Intent::OpenSource { path, line }`.
//!
//! The configured editor first (a command with `{path}` and `{line}` in it,
//! when Settings has one), then `code -g path:line`, then `zed path:line`,
//! then whatever the platform opens the file with. Nothing here runs a
//! command itself: it goes through a [`Launch`], so a test installs a
//! recorder and reads the commands it would have run.

use gpui::{App, Global};
use std::rc::Rc;

/// One command: a program and its arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Command {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

/// Runs commands.
pub(crate) trait Launch {
    /// Runs `command`: `Ok` when it started, the reason when it did not.
    fn run(&self, command: &Command) -> std::io::Result<()>;
}

/// The system: spawns the process and does not wait for it.
pub(crate) struct System;

impl Launch for System {
    fn run(&self, command: &Command) -> std::io::Result<()> {
        std::process::Command::new(&command.program)
            .args(&command.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
    }
}

/// The launcher a window opens sources through (the system's unless a test
/// or a harness installed another).
#[derive(Clone)]
pub(crate) struct Launcher(pub Rc<dyn Launch>);

impl Global for Launcher {}

/// Installs `launch` as the launcher.
pub(crate) fn install(launch: Rc<dyn Launch>, cx: &mut App) {
    cx.set_global(Launcher(launch));
}

/// The launcher in force.
pub(crate) fn launcher(cx: &App) -> Rc<dyn Launch> {
    cx.try_global::<Launcher>().map_or_else(|| Rc::new(System) as Rc<dyn Launch>, |launcher| Rc::clone(&launcher.0))
}

/// The commands to try, in order, to open `path` at `line`.
pub(crate) fn commands(configured: Option<&str>, path: &str, line: u32) -> Vec<Command> {
    let mut out = Vec::new();
    if let Some(template) = configured.map(str::trim).filter(|t| !t.is_empty()) {
        // Each word of the template is one argument, filled on its own: a
        // path with a space in it stays the one argument it is.
        let mut words = template.split_whitespace();
        if let Some(program) = words.next() {
            let mut args: Vec<String> = words.map(|word| word.replace("{path}", path).replace("{line}", &line.to_string())).collect();
            if !template.contains("{path}") {
                args.push(format!("{path}:{line}"));
            }
            out.push(Command { program: program.to_owned(), args });
        }
    }
    out.push(Command { program: "code".to_owned(), args: vec!["-g".to_owned(), format!("{path}:{line}")] });
    out.push(Command { program: "zed".to_owned(), args: vec![format!("{path}:{line}")] });
    out.push(Command { program: if cfg!(target_os = "macos") { "open" } else { "xdg-open" }.to_owned(), args: vec![path.to_owned()] });
    out
}

/// A launcher can prove only whether a process started. The external editor
/// may still refuse the file after that point, so success is a request, not
/// an assertion that an editor window opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchAttempt {
    pub command: Command,
    pub failure: Option<std::io::ErrorKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchOutcome {
    pub attempts: Vec<LaunchAttempt>,
}

impl LaunchOutcome {
    pub fn requested(&self) -> Option<&Command> {
        self.attempts.iter().find(|attempt| attempt.failure.is_none()).map(|attempt| &attempt.command)
    }

    pub fn message(&self, path: &str, line: u32) -> String {
        if let Some(command) = self.requested() {
            format!("Requested {} to open {path}:{line}. Check the external editor for the result.", command.program)
        } else {
            let reasons = self.attempts.iter().filter_map(|attempt| attempt.failure.map(|failure|
                format!("{}: {failure}", attempt.command.program))).collect::<Vec<_>>().join("; ");
            format!("Could not start an editor for {path}:{line}. {reasons}")
        }
    }
}

/// Opens `path` at `line`: the first process that starts wins. Every failed
/// start remains visible in the outcome for the visit-scoped product notice.
pub(crate) fn open(launch: &dyn Launch, configured: Option<&str>, path: &str, line: u32) -> LaunchOutcome {
    let mut attempts = Vec::new();
    for command in commands(configured, path, line) {
        let failure = launch.run(&command).err().map(|error| error.kind());
        let started = failure.is_none();
        attempts.push(LaunchAttempt { command, failure });
        if started {
            break;
        }
    }
    LaunchOutcome { attempts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records what would have run; starts only the programs in `works`.
    struct Recorder {
        ran: RefCell<Vec<Command>>,
        works: Vec<&'static str>,
    }

    impl Launch for Recorder {
        fn run(&self, command: &Command) -> std::io::Result<()> {
            self.ran.borrow_mut().push(command.clone());
            if self.works.contains(&command.program.as_str()) { Ok(()) } else { Err(std::io::ErrorKind::NotFound.into()) }
        }
    }

    #[test]
    fn code_first_at_the_line() {
        let recorder = Recorder { ran: RefCell::new(Vec::new()), works: vec!["code"] };
        let outcome = open(&recorder, None, "/w/crates/engine/src/lib.rs", 40);
        assert_eq!(outcome.requested(), Some(&Command { program: "code".into(), args: vec!["-g".into(), "/w/crates/engine/src/lib.rs:40".into()] }));
        assert!(outcome.message("/w/crates/engine/src/lib.rs", 40).starts_with("Requested code"));
    }

    #[test]
    fn then_zed_then_the_platform() {
        let recorder = Recorder { ran: RefCell::new(Vec::new()), works: vec!["zed"] };
        let outcome = open(&recorder, None, "/w/a.rs", 7);
        assert_eq!(outcome.attempts.iter().map(|attempt| attempt.command.program.as_str()).collect::<Vec<_>>(), ["code", "zed"]);
        assert_eq!(outcome.attempts[1].command.args, ["/w/a.rs:7"]);
        let none = Recorder { ran: RefCell::new(Vec::new()), works: vec![] };
        let outcome = open(&none, None, "/w/a.rs", 7);
        assert_eq!(outcome.attempts.len(), 3, "code, zed, the platform's opener: all tried");
        assert_eq!(outcome.attempts[2].command.args, ["/w/a.rs"]);
        assert!(outcome.requested().is_none());
        assert!(outcome.message("/w/a.rs", 7).starts_with("Could not start an editor"));
    }

    #[test]
    fn a_configured_editor_goes_first_with_its_own_template() {
        let recorder = Recorder { ran: RefCell::new(Vec::new()), works: vec!["subl"] };
        let outcome = open(&recorder, Some("subl {path}:{line}"), "/w/a.rs", 12);
        assert_eq!(outcome.requested(), Some(&Command { program: "subl".into(), args: vec!["/w/a.rs:12".into()] }));
        let bare = commands(Some("vim"), "/w/a.rs", 3);
        assert_eq!(bare[0], Command { program: "vim".into(), args: vec!["/w/a.rs:3".into()] });
        let spaced = commands(Some("subl {path}:{line}"), "/w/my project/a.rs", 3);
        assert_eq!(spaced[0].args, ["/w/my project/a.rs:3"], "a path with a space is one argument");
    }
}
