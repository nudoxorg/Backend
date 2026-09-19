//! Running a palette command in the window's own idiom.
//! A command that reads a declaration opens its page; one that adds a project
//! starts the add flow; one that browses opens the browse page. Only the
//! commands whose answer is genuinely a list of lines fall through to the
//! sheet's inline reply.
//!
//! Arguments come from the field: `> show ferris` resolves `ferris` on the
//! shelf, and a bare `> show` acts on the page being read. That is the same
//! rule the CLI uses for its optional operands, spelled for a window: the
//! thing under the reader's eye is the default operand.

use super::workspace::Workspace;
use crate::store::document::{Subject, Target};
use crate::store::search::CommandRow;
use crate::store::shell::SettingsPage;
use backend_library::{CommandId, SymbolKey};
use backend_present::IdentityKey;
use gpui::{Context, Focusable as _, Window};

impl Workspace {
    /// Runs one command the window can answer by opening something.
    ///
    /// Returns `false` when the command is not one of those, so the caller
    /// can let the sheet run it for its inline reply.
    pub(super) fn run_palette(
        &mut self,
        row: CommandRow,
        arguments: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let handled = match row.spec().id {
            CommandId::Show | CommandId::Document | CommandId::Read | CommandId::Related | CommandId::Graph => {
                self.open_named(arguments, false, cx)
            }
            CommandId::Source => self.open_named(arguments, true, cx),
            CommandId::Add => self.palette_add(arguments, window, cx),
            CommandId::Remove => self.palette_remove(arguments, cx),
            CommandId::Outline => self.palette_project(arguments, cx),
            CommandId::Search | CommandId::Name | CommandId::Resolve => {
                self.set_field(arguments.to_owned(), cx);
                true
            }
            CommandId::Packages | CommandId::Explore | CommandId::IndexSearch => {
                self.open_home(cx);
                true
            }
            CommandId::Package
            | CommandId::PackageProfile
            | CommandId::PackageVersions
            | CommandId::Dependents => self.palette_package(arguments, cx),
            CommandId::Health | CommandId::Revision => {
                self.shell.update(cx, |shell, cx| {
                    shell.show_settings_page(SettingsPage::Diagnostics, cx);
                });
                true
            }
            _ => false,
        };
        if handled {
            self.set_field(String::new(), cx);
            window.focus(&self.focus_handle(cx), cx);
        }
        handled
    }

    /// Opens the declaration an argument names, or the one being read.
    fn open_named(&mut self, arguments: &str, source: bool, cx: &mut Context<Self>) -> bool {
        let Some(symbol) = self.resolve_argument(arguments, cx) else {
            return false;
        };
        if source {
            self.unfurl("source", cx);
        }
        self.open_symbol(symbol, Target::Here, cx);
        true
    }

    /// Resolves a palette operand: a coordinate, a name on the shelf, or nothing
    /// for the page being read.
    fn resolve_argument(&self, arguments: &str, cx: &Context<Self>) -> Option<SymbolKey> {
        let wanted = arguments.trim();
        if wanted.is_empty() {
            return match self.document.read(cx).tab()?.subject()? {
                Subject::Declaration { symbol, .. } => Some(*symbol),
                Subject::Home | Subject::Project { .. } | Subject::Package { .. } => None,
            };
        }
        if wanted.contains("::") {
            return self.index.read(cx).symbol_for(wanted);
        }
        let near = self
            .active_identity(cx)
            .and_then(|identity| identity.project().map(|project| project.root().to_owned()));
        match self.index.read(cx).resolve_name(wanted, near.as_deref())?.1 {
            IdentityKey::Symbol(symbol) => Some(symbol),
            IdentityKey::Package(_) | IdentityKey::Absent => None,
        }
    }

    fn palette_add(&mut self, arguments: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if arguments.trim().is_empty() {
            self.begin_add(window, cx);
            return true;
        }
        match super::library::validate(arguments) {
            Ok(coordinate) => {
                self.index_project(coordinate, cx);
                true
            }
            Err(_) => false,
        }
    }

    fn palette_remove(&mut self, arguments: &str, cx: &mut Context<Self>) -> bool {
        let Some(coordinate) = self.project_argument(arguments, cx) else {
            return false;
        };
        self.remove_project(coordinate, cx);
        true
    }

    fn palette_project(&mut self, arguments: &str, cx: &mut Context<Self>) -> bool {
        let Some(coordinate) = self.project_argument(arguments, cx) else {
            return false;
        };
        self.open_project(coordinate, cx);
        true
    }

    /// Resolves a project operand: a name on the shelf, a coordinate, or the
    /// project of the page being read.
    fn project_argument(&self, arguments: &str, cx: &Context<Self>) -> Option<String> {
        let wanted = arguments.trim();
        if wanted.is_empty() {
            return self
                .document
                .read(cx)
                .tab()
                .and_then(|tab| tab.subject())
                .and_then(Subject::project_root);
        }
        let merged = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        merged
            .entries()
            .iter()
            .find(|entry| {
                entry.identity().name() == wanted
                    || entry.identity().coordinate().as_str() == wanted
            })
            .map(|entry| entry.identity().coordinate().as_str().to_owned())
    }

    fn palette_package(&mut self, arguments: &str, cx: &mut Context<Self>) -> bool {
        let wanted = arguments.trim();
        if wanted.starts_with("pkg:") {
            self.open_package(wanted.to_owned(), Target::Here, cx);
            return true;
        }
        false
    }
}
