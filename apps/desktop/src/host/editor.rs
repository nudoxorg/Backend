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

/// Opens `path` at `line`: the first command that starts wins. The commands
/// it tried, in order, are returned.
pub(crate) fn open(launch: &dyn Launch, configured: Option<&str>, path: &str, line: u32) -> Vec<Command> {
    let mut tried = Vec::new();
    for command in commands(configured, path, line) {
        let started = launch.run(&command).is_ok();
        tried.push(command);
        if started {
            break;
        }
    }
    tried
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
        let tried = open(&recorder, None, "/w/crates/engine/src/lib.rs", 40);
        assert_eq!(tried, [Command { program: "code".into(), args: vec!["-g".into(), "/w/crates/engine/src/lib.rs:40".into()] }]);
    }

    #[test]
    fn then_zed_then_the_platform() {
        let recorder = Recorder { ran: RefCell::new(Vec::new()), works: vec!["zed"] };
        let tried = open(&recorder, None, "/w/a.rs", 7);
        assert_eq!(tried.iter().map(|c| c.program.as_str()).collect::<Vec<_>>(), ["code", "zed"]);
        assert_eq!(tried[1].args, ["/w/a.rs:7"]);
        let none = Recorder { ran: RefCell::new(Vec::new()), works: vec![] };
        let tried = open(&none, None, "/w/a.rs", 7);
        assert_eq!(tried.len(), 3, "code, zed, the platform's opener: all tried");
        assert_eq!(tried[2].args, ["/w/a.rs"]);
    }

    #[test]
    fn a_configured_editor_goes_first_with_its_own_template() {
        let recorder = Recorder { ran: RefCell::new(Vec::new()), works: vec!["subl"] };
        let tried = open(&recorder, Some("subl {path}:{line}"), "/w/a.rs", 12);
        assert_eq!(tried, [Command { program: "subl".into(), args: vec!["/w/a.rs:12".into()] }]);
        let bare = commands(Some("vim"), "/w/a.rs", 3);
        assert_eq!(bare[0], Command { program: "vim".into(), args: vec!["/w/a.rs:3".into()] });
        let spaced = commands(Some("subl {path}:{line}"), "/w/my project/a.rs", 3);
        assert_eq!(spaced[0].args, ["/w/my project/a.rs:3"], "a path with a space is one argument");
    }
}
