//! Per-language proof that structural extraction carries real containment.
//!
//! The declaration floor, the containment rules, and the documentation a
//! reader wrote are decided per grammar, so they can only be proven against
//! the real compiled query of the real frontend: a fixture parsed here fails
//! if a capture name drifts from its grammar, if a supplementary pattern
//! stops selecting, if an `impl` block, receiver, or nested body stops
//! attaching, or if a grammar's own comment node stops being read. The
//! assertions are on rendered `(kind, name, container)` tuples and on exact
//! prose rather than on counts, because a count is equally happy when every
//! field inside the records is erased.

use backend_compile::{Container, DeclarationKind, SourceDeclaration, SyntaxError, SyntaxFrontend};
use std::{error::Error, path::Path};

/// Renders one declaration exactly as the assertions below read it.
fn rendered(declaration: &SourceDeclaration) -> String {
    let container = match declaration.container() {
        Container::Module => "module".to_owned(),
        Container::Enclosing { name, line } => format!("in {name}@{line}"),
        Container::Attached { type_name } => format!("on {type_name}"),
    };
    format!(
        "{} {} {container}",
        declaration.kind().name(),
        declaration.name()
    )
}

fn analyzed(
    frontend: &Result<SyntaxFrontend, SyntaxError>,
    path: &str,
    source: &str,
) -> Result<Vec<SourceDeclaration>, Box<dyn Error>> {
    let frontend = frontend.as_ref().map_err(ToString::to_string)?;
    let analysis = frontend.analyze(Path::new(path), source.as_bytes())?;
    Ok(analysis.declarations().to_vec())
}

/// Fails with the complete rendered extraction when an expectation is absent.
fn assert_declared(
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

/// Requires the exact prose a reader wrote above, or inside, a declaration.
fn assert_documented(
    declarations: &[SourceDeclaration],
    expected: &[(&str, &str, &str)],
) -> Result<(), Box<dyn Error>> {
    for (kind, name, documentation) in expected {
        let found = declarations
            .iter()
            .find(|declaration| declaration.name() == *name && declaration.kind().name() == *kind)
            .ok_or_else(|| format!("no {kind} named {name}"))?;
        if found.documentation() != *documentation {
            return Err(format!(
                "{kind} {name} documented {:?}, expected {documentation:?}",
                found.documentation()
            )
            .into());
        }
    }
    Ok(())
}

/// Requires the exact signature text retained for one declaration.
fn assert_signature(
    declarations: &[SourceDeclaration],
    kind: &str,
    name: &str,
    signature: &str,
) -> Result<(), Box<dyn Error>> {
    let found = declarations
        .iter()
        .find(|declaration| declaration.name() == name && declaration.kind().name() == kind)
        .ok_or_else(|| format!("no {kind} named {name}"))?;
    if found.signature() != signature {
        return Err(format!(
            "{kind} {name} signed {:?}, expected {signature:?}",
            found.signature()
        )
        .into());
    }
    Ok(())
}

const RUST_SOURCE: &str = r#"
pub struct Worker {
    /// The worker's display name.
    pub name: String,
    retries: u32,
}

/// Every event a worker reports.
pub enum Event {
    /// The worker began.
    Started,
    #[allow(dead_code)]
    /// Both the kind and the direction are known.
    Typed(SemanticLinkKind, RelationDirection),
}

pub const LIMIT: u32 = 3;

impl Worker {
    pub const NAME: &str = "worker";

    /// Runs the worker once.
    pub fn run(&self) -> u32 {
        self.retries
    }
}

pub trait Service {
    type Output;

    fn execute(&self) -> Self::Output;
}

pub mod inner {
    pub fn nested() {}
}
"#;

#[test]
fn rust_fields_variants_and_impl_methods_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_rust::syntax_frontend(),
        "worker.rs",
        RUST_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "struct Worker module",
            "field name in Worker@2",
            "field retries in Worker@2",
            "enum Event module",
            "variant Started in Event@9",
            "variant Typed in Event@9",
            "constant LIMIT module",
            "constant NAME on Worker",
            "method run on Worker",
            "trait Service module",
            "type Output in Service@28",
            "method execute in Service@28",
            "module inner module",
            "function nested in inner@34",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("field", "name", "The worker's display name."),
            ("enum", "Event", "Every event a worker reports."),
            ("variant", "Started", "The worker began."),
            (
                "variant",
                "Typed",
                "Both the kind and the direction are known.",
            ),
            ("method", "run", "Runs the worker once."),
        ],
    )?;
    assert_signature(
        &declarations,
        "variant",
        "Typed",
        "Typed(SemanticLinkKind, RelationDirection)",
    )
}

const PYTHON_SOURCE: &str = r"
class Worker:
    'Runs work items.'
    name = 'worker'
    # How many times to retry.
    retries = 3

    def run(self):
        'Runs once.'
        return self.retries

    @property
    def label(self):
        return self.name

def execute():
    return Worker().run()
";

#[test]
fn python_class_members_and_properties_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_python::syntax_frontend(),
        "worker.py",
        PYTHON_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "class Worker module",
            "field name in Worker@2",
            "field retries in Worker@2",
            "method run in Worker@2",
            "property label in Worker@2",
            "function execute module",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("class", "Worker", "Runs work items."),
            ("field", "retries", "How many times to retry."),
            ("method", "run", "Runs once."),
        ],
    )
}

const TYPESCRIPT_SOURCE: &str = r"
export interface Service {
  /** The stable identity. */
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
  /** The worker began. */
  Started,
  Stopped = 'stopped',
}

export function execute(): string {
  return 'done';
}
";

#[test]
fn typescript_members_and_enum_variants_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_typescript::syntax_frontend(),
        "worker.ts",
        TYPESCRIPT_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "interface Service module",
            "property id in Service@2",
            "method run in Service@2",
            "class Worker module",
            "field id in Worker@8",
            "method run in Worker@8",
            "enum Event module",
            "variant Started in Event@16",
            "variant Stopped in Event@16",
            "function execute module",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("property", "id", "The stable identity."),
            ("variant", "Started", "The worker began."),
        ],
    )
}

const GO_SOURCE: &str = r"
package worker

const Limit = 3

var Registry = 0

type Worker struct {
	// Name is the display name.
	Name    string
	retries int
}

type Service interface {
	Execute() int
}

// Run returns the retry count.
func (w *Worker) Run() int {
	return w.retries
}

func Execute() int {
	return 0
}
";

#[test]
fn go_struct_fields_and_receivers_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_go::syntax_frontend(),
        "worker.go",
        GO_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "constant Limit module",
            "variable Registry module",
            "type Worker module",
            "field Name in Worker@8",
            "field retries in Worker@8",
            "type Service module",
            "method Execute in Service@14",
            "method Run on Worker",
            "function Execute module",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("field", "Name", "Name is the display name."),
            ("method", "Run", "Run returns the retry count."),
        ],
    )
}

const JAVA_SOURCE: &str = r"
class Worker implements Service {
    /** The display name. */
    private final String name;
    private int retries;

    Worker(String name) {
        this.name = name;
    }

    public int run() {
        return retries;
    }
}

interface Service {
    int run();
}

enum Event {
    /** The worker began. */
    STARTED,
    STOPPED,
}
";

#[test]
fn java_fields_constructors_and_enum_constants_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_java::syntax_frontend(),
        "Worker.java",
        JAVA_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "class Worker module",
            "field name in Worker@2",
            "field retries in Worker@2",
            "constructor Worker in Worker@2",
            "method run in Worker@2",
            "interface Service module",
            "method run in Service@16",
            "enum Event module",
            "variant STARTED in Event@20",
            "variant STOPPED in Event@20",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("field", "name", "The display name."),
            ("variant", "STARTED", "The worker began."),
        ],
    )
}

const CSHARP_SOURCE: &str = r"
namespace Fixture
{
    class Worker
    {
        /// The retry count.
        private int retries;

        /// The display name.
        public string Name { get; set; }

        public Worker(int retries)
        {
            this.retries = retries;
        }

        public int Run()
        {
            return retries;
        }
    }

    struct Point
    {
        public int X;
    }

    enum Event
    {
        /// The worker began.
        Started,
        Stopped,
    }
}
";

#[test]
fn csharp_fields_properties_and_enum_members_carry_containment() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_csharp::syntax_frontend(),
        "Worker.cs",
        CSHARP_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "module Fixture module",
            "class Worker in Fixture@2",
            "field retries in Worker@4",
            "property Name in Worker@4",
            "constructor Worker in Worker@4",
            "method Run in Worker@4",
            "struct Point in Fixture@2",
            "field X in Point@23",
            "enum Event in Fixture@2",
            "variant Started in Event@28",
            "variant Stopped in Event@28",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("field", "retries", "The retry count."),
            ("property", "Name", "The display name."),
            ("variant", "Started", "The worker began."),
        ],
    )
}

const CLANG_SOURCE: &str = r"
namespace fixture {

struct Worker {
  /// The retry count.
  int retries;
  char *name;

  int run();
};

enum Event {
  /// The worker began.
  Started,
  Stopped,
};

int Worker::run() { return retries; }

}
";

#[test]
fn clang_members_enumerators_and_out_of_line_methods_carry_containment()
-> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_clang::syntax_frontend(),
        "worker.cpp",
        CLANG_SOURCE,
    )?;
    assert_declared(
        &declarations,
        &[
            "module fixture module",
            "struct Worker in fixture@2",
            "field retries in Worker@4",
            "field name in Worker@4",
            "enum Event in fixture@2",
            "variant Started in Event@12",
            "variant Stopped in Event@12",
            "method run on Worker",
        ],
    )?;
    assert_documented(
        &declarations,
        &[
            ("field", "retries", "The retry count."),
            ("variant", "Started", "The worker began."),
        ],
    )
}

#[test]
fn every_declaration_kind_is_named_by_the_shared_vocabulary() {
    assert_eq!(
        DeclarationKind::from_name("variant"),
        DeclarationKind::Variant
    );
    assert_eq!(
        DeclarationKind::from_name("enum_member"),
        DeclarationKind::Variant
    );
    assert_eq!(DeclarationKind::Variant.name(), "variant");
}
