//! Per-language proof that the structural lane names every declaration with
//! the one word the product vocabulary has for it, and that it finds the
//! declarations a reader navigates to even when a macro writes them.
//!
//! Each fixture goes through the real compiled tags query of the real
//! frontend. The assertions read back `(kind, name, container)` tuples, so a
//! method reported as a function, a class reported as a struct, a variant
//! reported as a constant, or a struct that disappears into a macro argument
//! fails here rather than on a page.

use backend_compile::{Container, SourceDeclaration, SyntaxError, SyntaxFrontend};
use std::{error::Error, path::Path};

fn analyzed(
    frontend: &Result<SyntaxFrontend, SyntaxError>,
    path: &str,
    source: &str,
) -> Result<Vec<SourceDeclaration>, Box<dyn Error>> {
    let frontend = frontend.as_ref().map_err(ToString::to_string)?;
    let analysis = frontend.analyze(Path::new(path), source.as_bytes())?;
    Ok(analysis.declarations().to_vec())
}

/// Renders one declaration as `kind name`, with the declaration holding it
/// (`in Holder`) or the type it attaches to (`on Type`) when there is one.
fn rendered(declaration: &SourceDeclaration) -> String {
    let container = match declaration.container() {
        Container::Module => String::new(),
        Container::Enclosing { name, .. } => format!(" in {name}"),
        Container::Attached { type_name } => format!(" on {type_name}"),
    };
    format!(
        "{} {}{container}",
        declaration.kind().name(),
        declaration.name()
    )
}

/// Requires every expected row, and fails with the whole extraction otherwise.
fn assert_rows(
    declarations: &[SourceDeclaration],
    expected: &[&str],
) -> Result<(), Box<dyn Error>> {
    let rows = declarations.iter().map(rendered).collect::<Vec<_>>();
    let missing = expected
        .iter()
        .filter(|entry| !rows.iter().any(|row| row == *entry))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!("missing {missing:?}\nextracted:\n  {}", rows.join("\n  ")).into())
}

/// Forbids rows that name a declaration with the wrong word.
fn assert_absent(
    declarations: &[SourceDeclaration],
    forbidden: &[&str],
) -> Result<(), Box<dyn Error>> {
    let rows = declarations.iter().map(rendered).collect::<Vec<_>>();
    let present = forbidden
        .iter()
        .filter(|entry| rows.iter().any(|row| row == *entry))
        .copied()
        .collect::<Vec<_>>();
    if present.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unexpected {present:?}\nextracted:\n  {}",
        rows.join("\n  ")
    )
    .into())
}

/// Returns the one-based line of the first line containing `needle`.
fn line_containing(source: &str, needle: &str) -> Option<u32> {
    source
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|index| u32::try_from(index + 1).ok())
}

fn line_of(declarations: &[SourceDeclaration], kind: &str, name: &str) -> Option<u32> {
    declarations
        .iter()
        .find(|declaration| declaration.kind().name() == kind && declaration.name() == name)
        .map(SourceDeclaration::line)
}

const GO_SOURCE: &str = r"
package kinds

type Level int

type Shade = Level

type Color struct {
	Name string
}

type Painter interface {
	Paint() Color
}

const Limit = 3

var Registry = 0

func (c *Color) Set() *Color {
	return c
}

func New() *Color {
	return &Color{}
}
";

#[test]
fn go_names_structs_interfaces_types_methods_and_functions() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_go::syntax_frontend(),
        "kinds.go",
        GO_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "type Level",
            "type Shade",
            "struct Color",
            "field Name in Color",
            "interface Painter",
            "method Paint in Painter",
            "constant Limit",
            "variable Registry",
            "method Set on Color",
            "function New",
        ],
    )?;
    assert_absent(&declarations, &["type Color", "type Painter"])
}

const PYTHON_SOURCE: &str = r#"
LIMIT = 3
counter = 0

class Worker:
    label: str = "x"

    def __init__(self):
        self.value = 1

    def run(self):
        def helper():
            return 1
        return helper()

    @property
    def size(self):
        return 1

    @staticmethod
    def make():
        return Worker()

def execute():
    return 0

if LIMIT:
    def where():
        return "a"
else:
    def where():
        return "b"
"#;

#[test]
fn python_names_classes_methods_properties_and_bindings() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_python::syntax_frontend(),
        "kinds.py",
        PYTHON_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "constant LIMIT",
            "variable counter",
            "class Worker",
            "field label in Worker",
            "method __init__ in Worker",
            "method run in Worker",
            "function helper in run",
            "property size in Worker",
            "method make in Worker",
            "function execute",
            "function where",
        ],
    )?;
    assert_absent(
        &declarations,
        &[
            "struct Worker",
            "function run in Worker",
            "constant counter",
            "function size in Worker",
        ],
    )?;
    let wheres = declarations
        .iter()
        .filter(|declaration| declaration.name() == "where")
        .count();
    assert_eq!(wheres, 2, "both branches define where");
    Ok(())
}

const TYPESCRIPT_SOURCE: &str = r"
export interface Service {
  readonly id: string;
  run(value: string): string;
}

export class Worker implements Service {
  readonly id: string = 'worker';
  run(value: string): string {
    return value;
  }
}

export enum Event {
  Started,
  Stopped = 'stopped',
}

export type Handler = (value: string) => void;

export const LIMIT = 3;
export let counter = 0;

export function execute(): string {
  return 'done';
}
";

#[test]
fn typescript_names_classes_interfaces_variants_aliases_and_bindings() -> Result<(), Box<dyn Error>>
{
    let declarations = analyzed(
        &backend_frontend_typescript::syntax_frontend(),
        "kinds.ts",
        TYPESCRIPT_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "interface Service",
            "property id in Service",
            "method run in Service",
            "class Worker",
            "field id in Worker",
            "method run in Worker",
            "enum Event",
            "variant Started in Event",
            "variant Stopped in Event",
            "type Handler",
            "constant LIMIT",
            "variable counter",
            "function execute",
        ],
    )?;
    assert_absent(
        &declarations,
        &["struct Worker", "constant counter", "constant Started"],
    )
}

const RUST_SOURCE: &str = r"
pub struct Plain {
    pub value: u8,
}

pub enum Event {
    Started,
    Typed(u8),
}

pub trait Service {
    type Output;
    const DEFAULT: u32;
    fn execute(&self) -> Self::Output;
}

pub type Alias = Plain;
pub const LIMIT: u32 = 3;
pub static GLOBAL: u32 = 4;

impl Plain {
    pub fn run(&self) {}
}

pub fn free() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}

    fn helper() {}
}
";

#[test]
fn rust_names_every_kind_and_keeps_test_only_items() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_rust::syntax_frontend(),
        "src/lib.rs",
        RUST_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "struct Plain",
            "field value in Plain",
            "enum Event",
            "variant Started in Event",
            "variant Typed in Event",
            "trait Service",
            "type Output in Service",
            "method execute in Service",
            "type Alias",
            "constant LIMIT",
            "variable GLOBAL",
            "method run on Plain",
            "function free",
            // Test-only code is code a reader navigates to.
            "module tests",
            "function it_works in tests",
            "function helper in tests",
        ],
    )?;
    assert_absent(
        &declarations,
        &["constant Started in Event", "function run on Plain"],
    )
}

const RUST_MACRO_SOURCE: &str = r#"
use pin_project_lite::pin_project;

pin_project! {
    /// A wrapper that adapts one interface to another.
    pub struct Compat<T> {
        #[pin]
        inner: T,
    }
}

impl<T> Compat<T> {
    pub fn get_ref(&self) -> &T {
        &self.inner
    }
}

thread_local! {
    static COUNTER: Cell<u32> = const { Cell::new(0) };
}

lazy_static! {
    /// The registered names.
    pub static ref REGISTRY: Vec<u8> = Vec::new();
}

bitflags! {
    /// Permissions on a file.
    pub struct Perms: u32 {
        /// Readable.
        const READ = 0b01;
        const WRITE = 0b10;
    }
}

cfg_if! {
    if #[cfg(unix)] {
        fn platform() {}
    } else {
        fn platform() {}
    }
}

mod outer {
    pin_project! {
        pub struct Inner {
            value: u8,
        }
    }
}

frobnicate! { widget "x" => fn thing() }

println!("fn main() {{}}");
"#;

#[test]
fn rust_indexes_items_declared_inside_item_macros() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_rust::syntax_frontend(),
        "src/compat.rs",
        RUST_MACRO_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "struct Compat",
            "field inner in Compat",
            "method get_ref on Compat",
            "variable COUNTER",
            "variable REGISTRY",
            "struct Perms",
            "constant READ in Perms",
            "constant WRITE in Perms",
            "function platform",
            // An item inside a macro inside a module belongs to that module.
            "module outer",
            "struct Inner in outer",
            "field value in Inner",
            // A macro whose argument is not Rust says what it could not index.
            "macro frobnicate!",
        ],
    )?;
    // Every re-parsed declaration sits on its own line of the original file.
    for (kind, name, needle) in [
        ("struct", "Compat", "pub struct Compat<T>"),
        ("field", "inner", "inner: T"),
        ("variable", "COUNTER", "static COUNTER"),
        ("struct", "Perms", "pub struct Perms"),
        ("constant", "WRITE", "const WRITE"),
        ("struct", "Inner", "pub struct Inner"),
        ("macro", "frobnicate!", "frobnicate!"),
    ] {
        assert_eq!(
            line_of(&declarations, kind, name),
            line_containing(RUST_MACRO_SOURCE, needle),
            "{kind} {name}"
        );
    }
    // The signature and excerpt are the real source, not the rewritten buffer.
    let perms = declarations
        .iter()
        .find(|declaration| declaration.name() == "Perms")
        .ok_or("no Perms")?;
    assert_eq!(perms.signature(), "pub struct Perms: u32");
    let registry = declarations
        .iter()
        .find(|declaration| declaration.name() == "REGISTRY")
        .ok_or("no REGISTRY")?;
    assert!(registry.signature().starts_with("pub static ref REGISTRY"));
    // Statement macros and macros that compute are not declarations.
    assert_absent(&declarations, &["macro println!", "macro pin_project!"])
}

#[test]
fn rust_macro_items_leave_ordinary_files_unchanged() -> Result<(), Box<dyn Error>> {
    let plain = analyzed(
        &backend_frontend_rust::syntax_frontend(),
        "src/lib.rs",
        "pub fn run() {\n    println!(\"hello\");\n}\n",
    )?;
    let rows = plain.iter().map(rendered).collect::<Vec<_>>();
    assert_eq!(rows, ["module lib", "function run"]);
    Ok(())
}

const JAVA_SOURCE: &str = r"
class Worker {
    Worker() {}
    void run() {}
}
interface Service {
    void run();
}
enum Event {
    STARTED
}
record Point(int x, int y) {}
";

#[test]
fn java_names_classes_interfaces_enums_records_and_constructors() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_java::syntax_frontend(),
        "Worker.java",
        JAVA_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "class Worker",
            "constructor Worker in Worker",
            "method run in Worker",
            "interface Service",
            "method run in Service",
            "enum Event",
            "variant STARTED in Event",
            "struct Point",
        ],
    )
}

const CSHARP_SOURCE: &str = r"
namespace Fixture
{
    class Worker { public Worker() {} public int Run() { return 1; } }
    struct Point { public int X; }
    interface IService { int Run(); }
    enum Event { Started }
}
";

#[test]
fn csharp_names_classes_structs_interfaces_and_enums() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_csharp::syntax_frontend(),
        "Worker.cs",
        CSHARP_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "class Worker in Fixture",
            "constructor Worker in Worker",
            "method Run in Worker",
            "struct Point in Fixture",
            "interface IService in Fixture",
            "enum Event in Fixture",
            "variant Started in Event",
        ],
    )
}

const C_SOURCE: &str = r"
struct Worker {
  int retries;
};
enum Event { Started, Stopped };
int run(struct Worker *worker) { return worker->retries; }
";

#[test]
fn clang_names_structs_enums_variants_and_functions() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_clang::syntax_frontend(),
        "worker.c",
        C_SOURCE,
    )?;
    assert_rows(
        &declarations,
        &[
            "struct Worker",
            "field retries in Worker",
            "enum Event",
            "variant Started in Event",
            "function run",
        ],
    )
}
